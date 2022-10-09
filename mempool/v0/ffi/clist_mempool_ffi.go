// This package provides a thin wrapper over the Rust FFI. 
// Friendly reminder: run `make build` at the root to build the rust bindings and header,
// and, for VSCode users, run "Reload Window"
package ffi

//#cgo LDFLAGS: -L${SRCDIR}/target/release -lclist_mempool_rs
// #include "target/release/mempool_bindings.h"
import "C"

type CListMempool struct {
	mempool *C.struct_CListMempool
}

func NewCListMempool() *CListMempool {
	return &CListMempool{
		mempool: (*C.struct_CListMempool)(C.CListMempool_new()),
	}
}
