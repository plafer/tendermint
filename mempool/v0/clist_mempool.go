// Friendly reminder: run `make build` at the root to build the rust bindings and header,
// and, for VSCode users, run "Reload Window"
package v0

//#cgo LDFLAGS: -L${SRCDIR}/ffi/target/release -lclist_mempool_rs
//#include "ffi/target/release/mempool_bindings.h"
import "C"

import (
	"errors"
	"fmt"
	"reflect"
	"sync"
	"sync/atomic"
	"unsafe"

	abci "github.com/tendermint/tendermint/abci/types"
	"github.com/tendermint/tendermint/config"
	"github.com/tendermint/tendermint/libs/clist"
	"github.com/tendermint/tendermint/libs/log"
	tmmath "github.com/tendermint/tendermint/libs/math"
	tmsync "github.com/tendermint/tendermint/libs/sync"
	"github.com/tendermint/tendermint/mempool"
	v0tx "github.com/tendermint/tendermint/mempool/v0/tx"
	"github.com/tendermint/tendermint/p2p"
	"github.com/tendermint/tendermint/proxy"
	"github.com/tendermint/tendermint/types"
)

// We're not allowed to store Go pointers in Rust, and Rust sometimes needs a
// reference to the Go part of the mempool e.g. to write to the `txsAvailable`
// channel. If we want to allow more than one mempool at a time, we can make
// this a slice and send a handle to Rust.
var gMem *CListMempool = nil

// CListMempool is an ordered in-memory pool for transactions before they are
// proposed in a consensus round. Transaction validity is checked using the
// CheckTx abci message before the transaction is added to the pool. The
// mempool uses a concurrent list structure for storing transactions that can
// be efficiently accessed by multiple concurrent readers.
type CListMempool struct {
	// Atomic integers
	height   int64 // the last block Update()'d to
	txsBytes int64 // total size of mempool, in bytes

	// notify listeners (ie. consensus) when txs are available
	notifiedTxsAvailable bool
	txsAvailable         chan struct{} // fires once for each height, when the mempool is not empty

	config *config.MempoolConfig

	// Exclusive mutex for Update method to prevent concurrent execution of
	// CheckTx or ReapMaxBytesMaxGas(ReapMaxTxs) methods.
	updateMtx tmsync.RWMutex
	preCheck  mempool.PreCheckFunc
	postCheck mempool.PostCheckFunc

	// Mutex to protect `Size()` and `SizeBytes()`
	// FIXME: find a better solution (e.g. atomics in Rust?)
	sizeMtx      tmsync.RWMutex
	addRemoveMtx tmsync.Mutex
	reqResCbMtx  tmsync.Mutex

	txs          *clist.CList // concurrent linked-list of good txs
	proxyAppConn proxy.AppConnMempool

	// Track whether we're rechecking txs.
	// These are not protected by a mutex and are expected to be mutated in
	// serial (ie. by abci responses which are called in serial).
	recheckCursor *clist.CElement // next expected response
	recheckEnd    *clist.CElement // re-checking stops here

	// Map for quick access to txs to record sender in CheckTx.
	// txsMap: txKey -> CElement
	txsMap sync.Map

	// Keep a cache of already-seen txs.
	// This reduces the pressure on the proxyApp.
	cache mempool.TxCache

	logger  log.Logger
	metrics *mempool.Metrics

	handle C.struct_Handle

	// Workaround the fact that function pointers can't be passed to Rust.
	// CheckTx
	checkTxTx     types.Tx
	checkTxCb     func(*abci.Response)
	checkTxTxInfo *mempool.TxInfo
	// resCbRecheck
	recheckResponseCheckTx *abci.ResponseCheckTx
}

var _ mempool.Mempool = &CListMempool{}

// CListMempoolOption sets an optional parameter on the mempool.
type CListMempoolOption func(*CListMempool)

// NewCListMempool returns a new mempool with the given configuration and
// connection to an application.
func NewCListMempool(
	cfg *config.MempoolConfig,
	proxyAppConn proxy.AppConnMempool,
	height int64,
	options ...CListMempoolOption,
) *CListMempool {

	if gMem != nil {
		// Currently can only have one instance at a time
		return nil
	}

	mp := &CListMempool{
		config:        cfg,
		proxyAppConn:  proxyAppConn,
		txs:           clist.New(),
		height:        height,
		recheckCursor: nil,
		recheckEnd:    nil,
		logger:        log.NewNopLogger(),
		metrics:       mempool.NopMetrics(),
		handle:        (C.struct_Handle)(C.clist_mempool_new(C.longlong(cfg.MaxTxBytes), C.longlong(cfg.Size), C.bool(cfg.KeepInvalidTxsInCache), C.bool(cfg.Recheck), C.longlong(height))),
	}

	if cfg.CacheSize > 0 {
		mp.cache = mempool.NewLRUTxCache(cfg.CacheSize)
	} else {
		mp.cache = mempool.NopTxCache{}
	}

	proxyAppConn.SetResponseCallback(mp.globalCb)

	for _, option := range options {
		option(mp)
	}

	gMem = mp

	return mp
}

// NOTE: not thread safe - should only be called once, on startup
func (mem *CListMempool) EnableTxsAvailable() {
	mem.txsAvailable = make(chan struct{}, 1)
}

// SetLogger sets the Logger.
func (mem *CListMempool) SetLogger(l log.Logger) {
	mem.logger = l
}

// WithPreCheck sets a filter for the mempool to reject a tx if f(tx) returns
// false. This is ran before CheckTx. Only applies to the first created block.
// After that, Update overwrites the existing value.
func WithPreCheck(f mempool.PreCheckFunc) CListMempoolOption {
	return func(mem *CListMempool) { mem.preCheck = f }
}

// WithPostCheck sets a filter for the mempool to reject a tx if f(tx) returns
// false. This is ran after CheckTx. Only applies to the first created block.
// After that, Update overwrites the existing value.
func WithPostCheck(f mempool.PostCheckFunc) CListMempoolOption {
	return func(mem *CListMempool) { mem.postCheck = f }
}

// WithMetrics sets the metrics.
func WithMetrics(metrics *mempool.Metrics) CListMempoolOption {
	return func(mem *CListMempool) { mem.metrics = metrics }
}

// Safe for concurrent use by multiple goroutines.
func (mem *CListMempool) Lock() {
	mem.updateMtx.Lock()
}

// Safe for concurrent use by multiple goroutines.
func (mem *CListMempool) Unlock() {
	mem.updateMtx.Unlock()
}

// Safe for concurrent use by multiple goroutines.
func (mem *CListMempool) Size() int {
	mem.sizeMtx.RLock()
	defer mem.sizeMtx.RUnlock()

	return (int)(C.clist_mempool_size(mem.handle))
}

// Safe for concurrent use by multiple goroutines.
func (mem *CListMempool) SizeBytes() int64 {
	mem.sizeMtx.RLock()
	defer mem.sizeMtx.RUnlock()

	return (int64)(C.clist_mempool_size_bytes(mem.handle))
}

// Lock() must be help by the caller during execution.
func (mem *CListMempool) FlushAppConn() error {
	return mem.proxyAppConn.FlushSync()
}

// XXX: Unsafe! Calling Flush may leave mempool in inconsistent state.
func (mem *CListMempool) Flush() {
	mem.updateMtx.RLock()
	defer mem.updateMtx.RUnlock()

	_ = atomic.SwapInt64(&mem.txsBytes, 0)
	mem.cache.Reset()

	for e := mem.txs.Front(); e != nil; e = e.Next() {
		mem.txs.Remove(e)
		e.DetachPrev()
	}

	mem.txsMap.Range(func(key, _ interface{}) bool {
		mem.txsMap.Delete(key)
		return true
	})
}

// TxsFront returns the first transaction in the ordered list for peer
// goroutines to call .NextWait() on.
// FIXME: leaking implementation details!
//
// Safe for concurrent use by multiple goroutines.
func (mem *CListMempool) TxsFront() *clist.CElement {
	return mem.txs.Front()
}

// TxsWaitChan returns a channel to wait on transactions. It will be closed
// once the mempool is not empty (ie. the internal `mem.txs` has at least one
// element)
//
// Safe for concurrent use by multiple goroutines.
func (mem *CListMempool) TxsWaitChan() <-chan struct{} {
	return mem.txs.WaitChan()
}

// It blocks if we're waiting on Update() or Reap().
// cb: A callback from the CheckTx command.
//
//	It gets called from another goroutine.
//
// CONTRACT: Either cb will get called, or err returned.
//
// Safe for concurrent use by multiple goroutines.
func (mem *CListMempool) CheckTx(
	tx types.Tx,
	cb func(*abci.Response),
	txInfo mempool.TxInfo,
) error {

	mem.updateMtx.RLock()
	// use defer to unlock mutex because application (*local client*) might panic
	defer mem.updateMtx.RUnlock()

	// Note: those are only needed by synchronous calls into Go that
	// `clist_mempool_check_tx()` will make
	mem.checkTxTx = tx
	mem.checkTxCb = cb
	mem.checkTxTxInfo = &txInfo

	raw_tx := C.struct_RawTx{
		tx:  (*C.uchar)(C.CBytes(tx)),
		len: (C.ulong)(len(tx)),
	}

	var err error = nil

	if C.clist_mempool_check_tx(mem.handle, raw_tx) {
		// FIXME: return proper errors from Rust
		err = mempool.ErrMempoolIsFull{
			NumTxs:      0,
			MaxTxs:      0,
			TxsBytes:    0,
			MaxTxsBytes: 0,
		}
	}

	C.free(unsafe.Pointer(raw_tx.tx))

	mem.checkTxTx = nil
	mem.checkTxCb = nil
	mem.checkTxTxInfo = nil

	return err
}

// Global callback that will be called after every ABCI response.
// Having a single global callback avoids needing to set a callback for each request.
// However, processing the checkTx response requires the peerID (so we can track which txs we heard from who),
// and peerID is not included in the ABCI request, so we have to set request-specific callbacks that
// include this information. If we're not in the midst of a recheck, this function will just return,
// so the request specific callback can do the work.
//
// When rechecking, we don't need the peerID, so the recheck callback happens
// here.
func (mem *CListMempool) globalCb(req *abci.Request, res *abci.Response) {
	if mem.recheckCursor == nil {
		// Q: this is the first time it was called?
		return
	}

	mem.metrics.RecheckTimes.Add(1)
	mem.resCbRecheck(req, res)

	// update metrics
	mem.metrics.Size.Set(float64(mem.Size()))
}

// Request specific callback that should be set on individual reqRes objects
// to incorporate local information when processing the response.
// This allows us to track the peer that sent us this tx, so we can avoid sending it back to them.
// NOTE: alternatively, we could include this information in the ABCI request itself.
//
// External callers of CheckTx, like the RPC, can also pass an externalCb through here that is called
// when all other response processing is complete.
//
// Used in CheckTx to record PeerID who sent us the tx.
func (mem *CListMempool) reqResCb(
	tx []byte,
	peerID uint16,
	peerP2PID p2p.ID,
	externalCb func(*abci.Response),
) func(res *abci.Response) {
	return func(res *abci.Response) {
		{
			mem.reqResCbMtx.Lock()
			defer mem.reqResCbMtx.Unlock()

			mem.resCbFirstTime(tx, peerID, peerP2PID, res)

			// update metrics
			mem.metrics.Size.Set(float64(mem.Size()))
		}

		// passed in by the caller of CheckTx, eg. the RPC
		if externalCb != nil {
			externalCb(res)
		}
	}
}

// Called from:
//   - resCbFirstTime (lock not held) if tx is valid
//
// FIXME: set back to `addTx` when CheckTx is implemented.
func (mem *CListMempool) AddTx(memTx *v0tx.MempoolTx) {
	mem.addRemoveMtx.Lock()
	defer mem.addRemoveMtx.Unlock()

	// FIXME: I'm not sure if the `CBytes` allocation is needed;
	// or whether Go's representation of a `[]byte` is the
	// same as C's (or rather if `&arr[0]` can be casted to unsafe.Pointer
	// and passed to C). Cgo docs confirm that you can pass pointers allocated
	// in Go to C, as long as C doesn't store them. But I'm not sure about the
	// type differences between `[]byte` and `uint8_t *`
	var c_tx = C.CBytes(memTx.Tx)
	C.clist_mempool_add_tx(mem.handle, C.longlong(memTx.Height), C.longlong(memTx.GasWanted), (*C.uchar)(c_tx), C.ulong(len(memTx.Tx)))
	C.free(c_tx)

	// metrics still tracked in go
	mem.metrics.TxSizeBytes.Observe(float64(len(memTx.Tx)))
}

// Called from:
//   - Update (lock held) if tx was committed
//   - resCbRecheck (lock not held) if tx was invalidated
//
// FIXME: set back to `addTx` when CheckTx is implemented.
func (mem *CListMempool) RemoveTx(tx types.Tx, elem *clist.CElement, removeFromCache bool) {
	mem.addRemoveMtx.Lock()
	defer mem.addRemoveMtx.Unlock()

	var c_tx = C.CBytes(tx)
	C.clist_mempool_remove_tx(mem.handle, (*C.uchar)(c_tx), C.ulong(len(tx)), C.bool(removeFromCache))
	C.free(c_tx)
}

// RemoveTxByKey removes a transaction from the mempool by its TxKey index.
func (mem *CListMempool) RemoveTxByKey(txKey types.TxKey) error {
	if e, ok := mem.txsMap.Load(txKey); ok {
		memTx := e.(*clist.CElement).Value.(*v0tx.MempoolTx)
		if memTx != nil {
			mem.RemoveTx(memTx.Tx, e.(*clist.CElement), false)
			return nil
		}
		return errors.New("transaction not found")
	}
	return errors.New("invalid transaction found")
}

func (mem *CListMempool) isFull(txSize int) error {
	if (bool)(C.clist_mempool_is_full(mem.handle, C.longlong(txSize))) {
		// FIXME: Fill in proper values
		return mempool.ErrMempoolIsFull{
			NumTxs:      0,
			MaxTxs:      mem.config.Size,
			TxsBytes:    0,
			MaxTxsBytes: mem.config.MaxTxsBytes,
		}
	}

	return nil
}

// callback, which is called after the app checked the tx for the first time.
//
// The case where the app checks the tx for the second and subsequent times is
// handled by the resCbRecheck callback.
func (mem *CListMempool) resCbFirstTime(
	tx []byte,
	peerID uint16,
	peerP2PID p2p.ID,
	res *abci.Response,
) {
	switch r := res.Value.(type) {
	case *abci.Response_CheckTx:
		var postCheckErr error = nil
		if mem.postCheck != nil {
			postCheckErr = mem.postCheck(tx, r.CheckTx)
		}

		hasPostCheckError := postCheckErr != nil
		raw_tx := C.struct_RawTx{
			tx:  (*C.uchar)(C.CBytes(tx)),
			len: (C.ulong)(len(tx)),
		}

		C.clist_mempool_res_cb_first_time(
			mem.handle, 
			C.uint(r.CheckTx.Code),
			C.bool(hasPostCheckError),
			raw_tx,
			C.longlong(r.CheckTx.GasWanted), 
			C.ushort(peerID),
		)

		C.free(unsafe.Pointer(raw_tx.tx))

	default:
		// ignore other messages
	}
}

// callback, which is called after the app rechecked the tx.
//
// The case where the app checks the tx for the first time is handled by the
// resCbFirstTime callback.
func (mem *CListMempool) resCbRecheck(req *abci.Request, res *abci.Response) {
	switch r := res.Value.(type) {
	case *abci.Response_CheckTx:
		// Workaround: cache values
		mem.recheckResponseCheckTx = r.CheckTx

		tx := req.GetCheckTx().Tx
		raw_tx := C.struct_RawTx{
			tx:  (*C.uchar)(C.CBytes(tx)),
			len: (C.ulong)(len(tx)),
		}

		C.clist_mempool_res_cb_recheck(mem.handle, C.uint(r.CheckTx.Code), raw_tx)

		C.free(unsafe.Pointer(raw_tx.tx))

		// Workaround: Clean cache
		mem.recheckResponseCheckTx = nil
	default:
		// ignore other messages
	}
}

// Safe for concurrent use by multiple goroutines.
func (mem *CListMempool) TxsAvailable() <-chan struct{} {
	return mem.txsAvailable
}

func (mem *CListMempool) notifyTxsAvailable() {
	if mem.Size() == 0 {
		panic("notified txs available but mempool is empty!")
	}
	if mem.txsAvailable != nil && !mem.notifiedTxsAvailable {
		// channel cap is 1, so this will send once
		mem.notifiedTxsAvailable = true
		select {
		case mem.txsAvailable <- struct{}{}:
		default:
		}
	}
}

// Safe for concurrent use by multiple goroutines.
func (mem *CListMempool) ReapMaxBytesMaxGas(maxBytes, maxGas int64) types.Txs {
	mem.updateMtx.RLock()
	defer mem.updateMtx.RUnlock()

	rust_raw_txs := C.clist_mempool_reap_max_bytes_max_gas(mem.handle, C.longlong(maxBytes), C.longlong(maxGas))
	rust_raw_txs_slice := unsafe.Slice(rust_raw_txs.txs, rust_raw_txs.len)

	txs := make([]types.Tx, len(rust_raw_txs_slice))
	for i := 0; i < len(txs); i++ {
		// allocate new memory since `raw_txs` is owned by Rust
		txs[i] = C.GoBytes(unsafe.Pointer(rust_raw_txs_slice[i].tx), C.int(rust_raw_txs_slice[i].len))
	}

	C.clist_mempool_raw_txs_free(mem.handle, rust_raw_txs)

	return txs
}

// Safe for concurrent use by multiple goroutines.
func (mem *CListMempool) ReapMaxTxs(max int) types.Txs {
	mem.updateMtx.RLock()
	defer mem.updateMtx.RUnlock()

	if max < 0 {
		max = mem.txs.Len()
	}

	txs := make([]types.Tx, 0, tmmath.MinInt(mem.txs.Len(), max))
	for e := mem.txs.Front(); e != nil && len(txs) <= max; e = e.Next() {
		memTx := e.Value.(*v0tx.MempoolTx)
		txs = append(txs, memTx.Tx)
	}
	return txs
}

// Lock() must be held by the caller during execution.
func (mem *CListMempool) Update(
	height int64,
	txs types.Txs,
	deliverTxResponses []*abci.ResponseDeliverTx,
	preCheck mempool.PreCheckFunc,
	postCheck mempool.PostCheckFunc,
) error {
	if preCheck != nil {
		mem.preCheck = preCheck
	}
	if postCheck != nil {
		mem.postCheck = postCheck
	}

	var raw_txs_slice = make([]C.struct_RawTx, len(txs))
	for i := 0; i < len(txs); i++ {
		raw_txs_slice[i] = C.struct_RawTx{
			// FIXME: I'm not sure if the `CBytes` allocation is needed
			tx:  (*C.uchar)(C.CBytes(txs[i])),
			len: (C.ulong)(len(txs[i])),
		}
	}

	var raw_txs = C.struct_RawTxs{
		// cgo quote:
		// >In C, a function argument written as a fixed size array
		// actually requires a pointer to the first element of the array. C
		// compilers are aware of this calling convention and adjust the call
		// accordingly, but Go cannot. In Go, you must pass the pointer to the
		// first element explicitly: C.f(&C.x[0]).
		//
		// In that example, they use data coming from C. However I *think* you
		// can also do it with a Go slice (i.e. that Go slice data is guaranteed
		// to be stored sequentially identically to C arrays)
		txs: &raw_txs_slice[0],
		len: (C.ulong)(len(raw_txs_slice)),
	}

	C.clist_mempool_update(mem.handle, C.longlong(height), raw_txs)

	// cleanup `CBytes` allocations
	for _, raw_tx := range raw_txs_slice {
		C.free(unsafe.Pointer(raw_tx.tx))
	}

	mem.metrics.Size.Set(float64(mem.Size()))

	return nil
}

// / Frees up the memory allocated in Rust for the mempool. The lack of destructors in Go makes FFI ugly.
// / Specifically, users of FFI types will need to manage Rust memory manually by making sure they
// / deallocate any memory they use. And ultimately all interfaces will need to add a `Free()` to ensure
// / that any concrete type that uses Rust in its implementation has a way to be cleaned up.
func (mem *CListMempool) Free() {
	C.clist_mempool_free(mem.handle)
	gMem = nil
}

//export rsNotifyTxsAvailable
func rsNotifyTxsAvailable() {
	gMem.notifyTxsAvailable()
}

//export rsMemPreCheck
func rsMemPreCheck() C.bool {
	if gMem.preCheck != nil {
		return gMem.preCheck(gMem.checkTxTx) != nil
	}

	// false if no error
	return false
}

//export rsMemPostCheck
func rsMemPostCheck(rawTx C.RawTx) C.bool {
	// Note: `C.GoBytes` makes a copy of the data, and the new memory is managed by Go
	// (i.e. garbage collected)
	tx := C.GoBytes(unsafe.Pointer(rawTx.tx), C.int(rawTx.len))
	if gMem.postCheck != nil {
		return gMem.postCheck(tx, gMem.recheckResponseCheckTx) != nil
	}

	// false if no error
	return false
}

//export rsMemProxyAppConnError
func rsMemProxyAppConnError() C.bool {
	// NOTE: proxyAppConn may error if tx buffer is full
	if err := gMem.proxyAppConn.Error(); err != nil {
		return true
	}

	return false
}

//export rsMemProxyAppConnCheckTxAsync
func rsMemProxyAppConnCheckTxAsync(rawTx C.RawTx, setCallback C.bool) {
	// Note: `C.GoBytes` makes a copy of the data, and the new memory is managed by Go
	// (i.e. garbage collected)
	tx := types.Tx(C.GoBytes(unsafe.Pointer(rawTx.tx), C.int(rawTx.len)))
	reqRes := gMem.proxyAppConn.CheckTxAsync(abci.RequestCheckTx{Tx: tx})

	if setCallback {
		// TODO: remove this check
		if !reflect.DeepEqual(tx, gMem.checkTxTx) {
			panic(fmt.Sprintf("tx not properly converted. \ntx: %v.\ncheckTxTx: %v", tx, gMem.checkTxTx))
		}

		reqRes.SetCallback(gMem.reqResCb(
			gMem.checkTxTx,
			gMem.checkTxTxInfo.SenderID,
			gMem.checkTxTxInfo.SenderP2PID,
			gMem.checkTxCb))
	}
}

//export rsMemProxyAppConnFlushAsync
func rsMemProxyAppConnFlushAsync() {
	gMem.proxyAppConn.FlushAsync()
}
