// Command gogt builds the Go ground truth: file -> file import edges.
//
// Packages, their directories and their file lists come from the Go
// toolchain (`go list -e -json ./...`, run by the caller with GOOS=linux
// GOARCH=amd64 and fed on stdin). Each listed file's own import paths come
// from go/parser (ImportsOnly). A file importing package P depends on every
// non-test file compiled into P (GoFiles + CgoFiles). Same-package references
// need no import and are not import edges.
//
// usage: go list -e -json ./... | gogt <repo> > out.json
package main

import (
	"encoding/json"
	"fmt"
	"go/parser"
	"go/token"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strconv"
)

type pkg struct {
	ImportPath   string
	Dir          string
	GoFiles      []string
	CgoFiles     []string
	TestGoFiles  []string
	XTestGoFiles []string
}

func main() {
	root, err := filepath.Abs(os.Args[1])
	if err != nil {
		panic(err)
	}
	dec := json.NewDecoder(os.Stdin)
	var pkgs []pkg
	for {
		var p pkg
		if err := dec.Decode(&p); err == io.EOF {
			break
		} else if err != nil {
			panic(err)
		}
		pkgs = append(pkgs, p)
	}
	rel := func(dir, f string) string {
		r, err := filepath.Rel(root, filepath.Join(dir, f))
		if err != nil {
			panic(err)
		}
		return filepath.ToSlash(r)
	}
	nonTest := map[string][]string{}
	var universe []string
	for _, p := range pkgs {
		for _, f := range append(append([]string{}, p.GoFiles...), p.CgoFiles...) {
			nonTest[p.ImportPath] = append(nonTest[p.ImportPath], rel(p.Dir, f))
		}
	}
	var edges [][2]string
	fset := token.NewFileSet()
	for _, p := range pkgs {
		all := append(append(append(append([]string{}, p.GoFiles...), p.CgoFiles...), p.TestGoFiles...), p.XTestGoFiles...)
		for _, f := range all {
			from := rel(p.Dir, f)
			universe = append(universe, from)
			file, err := parser.ParseFile(fset, filepath.Join(p.Dir, f), nil, parser.ImportsOnly)
			if err != nil {
				fmt.Fprintln(os.Stderr, "parse:", err)
				continue
			}
			seen := map[string]bool{}
			for _, imp := range file.Imports {
				path, _ := strconv.Unquote(imp.Path.Value)
				for _, to := range nonTest[path] {
					if to != from && !seen[to] {
						seen[to] = true
						edges = append(edges, [2]string{from, to})
					}
				}
			}
		}
	}
	sort.Strings(universe)
	out := map[string]any{
		"tool":     "go list -e -json ./... (GOOS=linux GOARCH=amd64) + go/parser ImportsOnly",
		"universe": universe,
		"edges":    edges,
		"stats":    map[string]any{"packages": len(pkgs)},
	}
	if err := json.NewEncoder(os.Stdout).Encode(out); err != nil {
		panic(err)
	}
}
