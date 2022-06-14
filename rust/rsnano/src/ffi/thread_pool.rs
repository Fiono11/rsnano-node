use std::{ffi::c_void, time::Duration};

use crate::ThreadPool;

pub struct FfiThreadPool {
    handle: *mut c_void,
}

impl FfiThreadPool {
    pub fn new(handle: *mut c_void) -> Self {
        Self { handle }
    }
}

impl ThreadPool for FfiThreadPool {
    fn add_timed_task(&self, delay: Duration, callback: Box<dyn FnOnce()>) {
        unsafe {
            match ADD_TIMED_TASK_CALLBACK {
                Some(f) => f(
                    self.handle,
                    delay.as_millis() as u64,
                    Box::into_raw(Box::new(VoidFnCallbackHandle::new(callback))),
                ),
                None => panic!("ADD_TIMED_TASK_CALLBACK missing"),
            }
        }
    }
}
pub struct VoidFnCallbackHandle(Option<Box<dyn FnOnce()>>);

impl VoidFnCallbackHandle {
    pub fn new(f: Box<dyn FnOnce()>) -> Self {
        VoidFnCallbackHandle(Some(f))
    }
}

type AddTimedTaskCallback = unsafe extern "C" fn(*mut c_void, u64, *mut VoidFnCallbackHandle);

static mut ADD_TIMED_TASK_CALLBACK: Option<AddTimedTaskCallback> = None;

#[no_mangle]
pub unsafe extern "C" fn rsn_callback_add_timed_task(f: AddTimedTaskCallback) {
    ADD_TIMED_TASK_CALLBACK = Some(f);
}

#[no_mangle]
pub unsafe extern "C" fn rsn_void_fn_callback_call(f: *mut VoidFnCallbackHandle) {
    if let Some(cb) = (*f).0.take() {
        cb();
    }
}

#[no_mangle]
pub unsafe extern "C" fn rsn_void_fn_callback_destroy(f: *mut VoidFnCallbackHandle) {
    drop(Box::from_raw(f))
}
