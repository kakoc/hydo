// retime IN OUT: copy a tar archive, with every entry's times set to SOURCE_DATE_EPOCH and its
// owner names cleared. mender-artifact archives its temporary header.tar.gz and data/0000.tar.gz
// files with their own file times, so each run gave another Artifact. Mender checks the
// contents of these entries (the manifest's sha256 sums), not their tar headers, so the copy is
// the same Artifact with the same bytes from every build.
package main

import (
	"archive/tar"
	"io"
	"os"
	"strconv"
	"time"
)

func main() {
	epoch, err := strconv.ParseInt(os.Getenv("SOURCE_DATE_EPOCH"), 10, 64)
	if err != nil {
		panic("SOURCE_DATE_EPOCH: " + err.Error())
	}
	t := time.Unix(epoch, 0)
	in, err := os.Open(os.Args[1])
	if err != nil {
		panic(err)
	}
	out, err := os.Create(os.Args[2])
	if err != nil {
		panic(err)
	}
	r := tar.NewReader(in)
	w := tar.NewWriter(out)
	for {
		h, err := r.Next()
		if err == io.EOF {
			break
		}
		if err != nil {
			panic(err)
		}
		h.ModTime, h.AccessTime, h.ChangeTime = t, time.Time{}, time.Time{}
		h.Uname, h.Gname, h.Uid, h.Gid = "", "", 0, 0
		h.Format = tar.FormatUnknown
		if err := w.WriteHeader(h); err != nil {
			panic(err)
		}
		if _, err := io.Copy(w, r); err != nil {
			panic(err)
		}
	}
	if err := w.Close(); err != nil {
		panic(err)
	}
	if err := out.Close(); err != nil {
		panic(err)
	}
}
