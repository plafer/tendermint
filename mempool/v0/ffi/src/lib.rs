
#[repr(C)]
pub struct CListMempool {

}

#[no_mangle]
pub extern "C" fn CListMempool_new() -> *mut CListMempool {
    todo!()
}

#[no_mangle]
pub extern "C" fn CListMempool_free(_mempool: *mut CListMempool) {
    todo!()
}
