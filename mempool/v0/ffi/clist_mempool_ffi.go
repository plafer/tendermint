// This package provides a thin wrapper over the Rust FFI.
// Friendly reminder: run `make build` at the root to build the rust bindings and header,
// and, for VSCode users, run "Reload Window"
package ffi

//#cgo LDFLAGS: -L${SRCDIR}/target/release -lclist_mempool_rs
// #include "target/release/mempool_bindings.h"
import "C"
import "unsafe"

import "github.com/tendermint/tendermint/config"
import "github.com/tendermint/tendermint/mempool/v0/tx"

type CListMempool struct {
	handle C.struct_Handle
}

func NewCListMempool(
	cfg *config.MempoolConfig,
	height int64,
) *CListMempool {
	return &CListMempool{
		handle: (C.struct_Handle)(C.clist_mempool_new(C.longlong(cfg.MaxTxBytes), C.longlong(cfg.Size), C.bool(cfg.KeepInvalidTxsInCache), C.bool(cfg.Recheck), C.longlong(height))),
	}
}

func (m CListMempool) Size() int {
	// Not sure if this is the best way to convert C's int type
	// to Go's int
	return (int)(C.clist_mempool_size(m.handle))
}

func (m CListMempool) SizeBytes() int64 {
	return (int64)(C.clist_mempool_size_bytes(m.handle))
}

func (m CListMempool) IsFull(txSize int) bool {
	return (bool)(C.clist_mempool_is_full(m.handle, C.longlong(txSize)))
}

func (m CListMempool) AddTx(memTx *tx.MempoolTx) {
	var c_tx = unsafe.Pointer(&memTx.Tx)
	C.clist_mempool_add_tx(m.handle, C.longlong(memTx.Height), C.longlong(memTx.GasWanted), (*C.uchar)(c_tx), C.ulong(len(memTx.Tx)))
}

// / Frees up the memory allocated in Rust for the mempool. The lack of destructors in Go makes FFI ugly.
// / Specifically, users of FFI types will need to manage Rust memory manually by making sure they
// / deallocate any memory they use. And ultimately all interfaces will need to add a `Free()` to ensure
// / that any concrete type that uses Rust in its implementation has a way to be cleaned up.
func (m CListMempool) Free() {
	C.clist_mempool_free(m.handle)
}
