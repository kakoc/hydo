// Package load is the host side of the generator: it reads a project's service definitions
// (.proto files, a compiled descriptor set, or a running server's reflection service) and
// its hints file, checks the hints against the schema, and produces the plan.
package load

import (
	"context"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/bufbuild/protocompile"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	rpb "google.golang.org/grpc/reflection/grpc_reflection_v1"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protodesc"
	"google.golang.org/protobuf/reflect/protoreflect"
	"google.golang.org/protobuf/types/descriptorpb"
)

// FromProtoDir compiles every .proto file under dir (recursively; import paths are dir plus
// extra). The well-known types (google/protobuf/*.proto) are built in.
func FromProtoDir(dir string, extra []string) (*descriptorpb.FileDescriptorSet, []string, error) {
	var names []string
	err := filepath.WalkDir(dir, func(p string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if !d.IsDir() && strings.HasSuffix(p, ".proto") {
			rel, _ := filepath.Rel(dir, p)
			names = append(names, filepath.ToSlash(rel))
		}
		return nil
	})
	if err != nil {
		return nil, nil, err
	}
	if len(names) == 0 {
		return nil, nil, fmt.Errorf("no .proto files under %s", dir)
	}
	sort.Strings(names)
	c := protocompile.Compiler{
		Resolver: protocompile.WithStandardImports(&protocompile.SourceResolver{ImportPaths: append([]string{dir}, extra...)}),
	}
	files, err := c.Compile(context.Background(), names...)
	if err != nil {
		return nil, nil, err
	}
	all := map[string]protoreflect.FileDescriptor{}
	var add func(fd protoreflect.FileDescriptor)
	add = func(fd protoreflect.FileDescriptor) {
		if _, ok := all[fd.Path()]; ok {
			return
		}
		all[fd.Path()] = fd
		imps := fd.Imports()
		for i := 0; i < imps.Len(); i++ {
			add(imps.Get(i).FileDescriptor)
		}
	}
	for _, f := range files {
		add(f)
	}
	var fdps []*descriptorpb.FileDescriptorProto
	for _, fd := range all {
		fdps = append(fdps, protodesc.ToFileDescriptorProto(fd))
	}
	return Canonical(fdps), names, nil
}

// FromDescriptorSet reads a FileDescriptorSet (protoc --include_imports -o, buf build -o).
func FromDescriptorSet(path string) (*descriptorpb.FileDescriptorSet, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var set descriptorpb.FileDescriptorSet
	if err := proto.Unmarshal(b, &set); err != nil {
		return nil, fmt.Errorf("%s: not a FileDescriptorSet: %v", path, err)
	}
	return Canonical(set.File), nil
}

// FromReflection asks a running server (gRPC server reflection, v1) for every service it
// exposes and the files that define them, with their imports.
func FromReflection(target string, timeout time.Duration) (*descriptorpb.FileDescriptorSet, error) {
	cc, err := grpc.NewClient(target, grpc.WithTransportCredentials(insecure.NewCredentials()))
	if err != nil {
		return nil, err
	}
	defer cc.Close()
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	st, err := rpb.NewServerReflectionClient(cc).ServerReflectionInfo(ctx)
	if err != nil {
		return nil, fmt.Errorf("reflection on %s: %v", target, err)
	}
	ask := func(req *rpb.ServerReflectionRequest) (*rpb.ServerReflectionResponse, error) {
		if err := st.Send(req); err != nil {
			return nil, err
		}
		resp, err := st.Recv()
		if err != nil {
			return nil, err
		}
		if e := resp.GetErrorResponse(); e != nil {
			return nil, fmt.Errorf("reflection error %d: %s", e.ErrorCode, e.ErrorMessage)
		}
		return resp, nil
	}
	resp, err := ask(&rpb.ServerReflectionRequest{MessageRequest: &rpb.ServerReflectionRequest_ListServices{ListServices: "*"}})
	if err != nil {
		return nil, fmt.Errorf("reflection on %s: %v (is server reflection enabled?)", target, err)
	}
	got := map[string]*descriptorpb.FileDescriptorProto{}
	take := func(r *rpb.ServerReflectionResponse) error {
		for _, b := range r.GetFileDescriptorResponse().GetFileDescriptorProto() {
			var fdp descriptorpb.FileDescriptorProto
			if err := proto.Unmarshal(b, &fdp); err != nil {
				return err
			}
			got[fdp.GetName()] = &fdp
		}
		return nil
	}
	for _, s := range resp.GetListServicesResponse().GetService() {
		if skipService(s.GetName()) {
			continue
		}
		r, err := ask(&rpb.ServerReflectionRequest{MessageRequest: &rpb.ServerReflectionRequest_FileContainingSymbol{FileContainingSymbol: s.GetName()}})
		if err != nil {
			return nil, fmt.Errorf("reflection: service %s: %v", s.GetName(), err)
		}
		if err := take(r); err != nil {
			return nil, err
		}
	}
	for changed := true; changed; {
		changed = false
		var missing []string
		for _, f := range got {
			for _, d := range f.GetDependency() {
				if _, ok := got[d]; !ok {
					missing = append(missing, d)
				}
			}
		}
		sort.Strings(missing)
		for _, d := range missing {
			if _, ok := got[d]; ok {
				continue
			}
			r, err := ask(&rpb.ServerReflectionRequest{MessageRequest: &rpb.ServerReflectionRequest_FileByFilename{FileByFilename: d}})
			if err != nil {
				return nil, fmt.Errorf("reflection: file %s: %v", d, err)
			}
			if err := take(r); err != nil {
				return nil, err
			}
			changed = true
		}
	}
	st.CloseSend()
	var fdps []*descriptorpb.FileDescriptorProto
	for _, f := range got {
		fdps = append(fdps, f)
	}
	return Canonical(fdps), nil
}

func skipService(name string) bool {
	return strings.HasPrefix(name, "grpc.reflection.") || strings.HasPrefix(name, "grpc.health.")
}

// Canonical orders files dependencies-first (ties by name) and drops source info, so the
// same schema always serializes to the same bytes.
func Canonical(fdps []*descriptorpb.FileDescriptorProto) *descriptorpb.FileDescriptorSet {
	byName := map[string]*descriptorpb.FileDescriptorProto{}
	var names []string
	for _, f := range fdps {
		f = proto.Clone(f).(*descriptorpb.FileDescriptorProto)
		f.SourceCodeInfo = nil
		if _, dup := byName[f.GetName()]; !dup {
			names = append(names, f.GetName())
		}
		byName[f.GetName()] = f
	}
	sort.Strings(names)
	done := map[string]bool{}
	var out []*descriptorpb.FileDescriptorProto
	var visit func(n string)
	visit = func(n string) {
		if done[n] {
			return
		}
		done[n] = true
		f, ok := byName[n]
		if !ok {
			return
		}
		deps := append([]string(nil), f.GetDependency()...)
		sort.Strings(deps)
		for _, d := range deps {
			visit(d)
		}
		out = append(out, f)
	}
	for _, n := range names {
		visit(n)
	}
	return &descriptorpb.FileDescriptorSet{File: out}
}

// Marshal serializes a set deterministically.
func Marshal(set *descriptorpb.FileDescriptorSet) ([]byte, error) {
	return proto.MarshalOptions{Deterministic: true}.Marshal(set)
}
