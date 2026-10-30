// Package schema loads the descriptor set the generator wrote (grpc/desc.pb) into a registry.
// It is the only descriptor code the guest client needs (no .proto parser, no YAML).
package schema

import (
	"fmt"
	"os"

	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protodesc"
	"google.golang.org/protobuf/reflect/protoreflect"
	"google.golang.org/protobuf/reflect/protoregistry"
	"google.golang.org/protobuf/types/descriptorpb"
)

// Read loads a serialized FileDescriptorSet.
func Read(path string) (*protoregistry.Files, error) {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var set descriptorpb.FileDescriptorSet
	if err := proto.Unmarshal(b, &set); err != nil {
		return nil, fmt.Errorf("%s: %v", path, err)
	}
	return protodesc.NewFiles(&set)
}

// Message finds a message type by full name.
func Message(files *protoregistry.Files, name string) (protoreflect.MessageDescriptor, error) {
	d, err := files.FindDescriptorByName(protoreflect.FullName(name))
	if err != nil {
		return nil, fmt.Errorf("message %s: %v", name, err)
	}
	md, ok := d.(protoreflect.MessageDescriptor)
	if !ok {
		return nil, fmt.Errorf("%s is not a message", name)
	}
	return md, nil
}
