# Learnings

## Pros
+ Allows us to extend tendermint in-process in Rust
    + Perhaps we can do cool stuff with hermes?

## Cons
+ The FFI boundary is very unsafe and many hard-to-catch bugs can occur
    + Seems like the worst of both go and rust worlds
    + See [Passing pointers](https://golang.google.cn/cmd/cgo/#hdr-Passing_pointers) section of cgo docs
    + e.g. "Go code may pass a Go pointer to C provided the Go memory to which it points does not contain any Go pointers." is easy to get wrong
        + You can enable runtime checks with `GODEBUG=cgocheck=1` though
+ The lack of destructors in Go makes FFI ugly. Specifically, users of FFI types
  will need to manage Rust memory manually by making sure they deallocate any
  memory they use. And ultimately all interfaces will need to add a `Free()` to
  ensure that any concrete type that uses Rust in its implementation has a way
  to be cleaned up.
  + e.g. see `CListMempool.Free()` which imposes `Mempool.Free()`
+ Some things need to be done in Go
    + e.g.
        + interaction with channels
        + call callback functions written in another module in go
    + Everytime this happens, we need to expose a function from go to rust for Rust to access that go feature
        + e.g. `rsNotifyTxsAvailable()`
    + Ugly, and increases the complexity of the logic
+ We're allowed to pass Go pointers across the ffi boundary, but we're not
  allowed to store the pointers after the function call returns.
  + This leads to extra allocations that weren't needed in the go-only code
  + We also need to store the current mempool in a go module variable, for when rust code needs to perform an operation with it
    + e.g. see cgo-exported `rs*` functions
+ Can no longer easily cross-compile
+ Use of go `unsafe` package
    + From package docs: Packages that import unsafe may be non-portable and are
      not protected by the Go 1 compatibility guidelines. 

# General notes
+ There are concurrent execution patterns from the go mempool design that violate rust's memory model
    + e.g. [here](https://github.com/tendermint/tendermint/blob/99a7ac84dca30676fd544be18c6df2880a14429f/mempool/v0/clist_mempool.go#L650). `resCbFirstTime` can be called concurrently with `Update`, and both borrow the `CListMempool` mutably in Rust.
    + In order to be confident that we never violate Rust's memory model, mutex logic should be cleaned up in go
        + e.g. [this](https://github.com/tendermint/tendermint/blob/99a7ac84dca30676fd544be18c6df2880a14429f/mempool/v0/clist_mempool.go#L578) or [this issue](https://github.com/tendermint/tendermint/issues/9525)


# Known shortcomings
Most mempool tests that I didn't write fail because a few things need to be implemented still:
+ Currently no cache implemented, and some tests expect that
+ We don't report proper errors across the ffi boundary (just `true`/`false`); some tests have logic based on which error happened
+ our `isFull()` implementation doesn't check the proper size in bytes. See comments in the code.
