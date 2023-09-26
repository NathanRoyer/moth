#![allow(dead_code)]

pub use lmfu;
pub use lmfu::json::{JsonFile, Path as JsonPath, Value as JsonValue};

use lmfu::{strpool::Pool, ArcStr};
use core::ptr::NonNull;

pub use moth_wasm_macros::moth_callback;

pub fn param(ptr: u64, len: u64) -> &'static str {
    use core::{slice, str};
    str::from_utf8(unsafe { slice::from_raw_parts(ptr as _, len as _) }).unwrap()
}

extern "C" {
    fn __read_table_entry(
        db_token: u64,
        in_tn_len: u64,
        in_tn_ptr: u64,
        in_key_len: u64,
        in_key_ptr: u64,
    ) -> /* out_json_ptr */ u64;

    fn __write_table_entry(
        db_token: u64,
        in_tn_len: u64,
        in_tn_ptr: u64,
        in_key_len: u64,
        in_key_ptr: u64,
        in_json_len: u64,
        in_json_ptr: u64,
    );

    fn __set_template_name(
        db_token: u64,
        in_name_len: u64,
        in_name_ptr: u64,
    );

    fn __set_template_param(
        db_token: u64,
        in_key_len: u64,
        in_key_ptr: u64,
        in_value_len: u64,
        in_value_ptr: u64,
    );
}

pub struct Request {
    db_token: u64,
    body: Option<Box<JsonFile>>,
}

impl Request {
    pub unsafe fn new(db_token: u64, body_ptr: u64) -> Self {
        Self {
            db_token,
            body: Some(Box::from_raw(body_ptr as *mut JsonFile)),
        }
    }

    pub fn take_body(&mut self) -> Box<JsonFile> {
        self.body.take().expect("Request body was already taken")
    }

    pub fn read_table_entry(&self, table: &str, key: &str) -> Option<Box<JsonFile>> {
        unsafe {
            let json_ptr = __read_table_entry(
                self.db_token,
                table.as_ptr() as _,
                table.len() as _,
                key.as_ptr() as _,
                key.len() as _,
            );

            match json_ptr {
                0 => None,
                p => Some(Box::from_raw(p as *mut JsonFile)),
            }
        }
    }

    pub fn write_table_entry(&self, table: &str, key: &str, json: &str) {
        unsafe {
            __write_table_entry(
                self.db_token,
                table.as_ptr() as _,
                table.len() as _,
                key.as_ptr() as _,
                key.len() as _,
                json.as_ptr() as _,
                json.len() as _,
            );
        }
    }

    pub fn set_template_name(&self, name: &str) {
        unsafe {
            __set_template_name(self.db_token, name.as_ptr() as _, name.len() as _);
        }
    }

    pub fn set_template_param(&self, key: &str, value: &str) {
        unsafe {
            __set_template_param(
                self.db_token,
                key.as_ptr() as _,
                key.len() as _,
                value.as_ptr() as _,
                value.len() as _,
            );
        }
    }
}

#[no_mangle]
extern "C" fn __rs_malloc(size: u64) -> /* ptr */ u64 {
    (Box::into_raw(vec![0u8; size as _].into_boxed_slice()) as *mut u8) as _
}

#[no_mangle]
extern "C" fn __rs_free(ptr: u64, size: u64) {
    let slice_ptr = core::ptr::slice_from_raw_parts_mut(ptr as *mut u8, size as _);
    unsafe { Box::from_raw(slice_ptr) };
}

#[no_mangle]
extern "C" fn __parse_json(in_str_ptr: u64, in_str_len: u64) -> /* out_json_ptr */ u64 {
    let string = param(in_str_ptr, in_str_len);
    match JsonFile::with_key_pool(Some(string), Pool::get_static_pool()) {
        Ok(parsed) => Box::into_raw(Box::new(parsed)) as _,
        Err(_) => 0,
    }
}

#[no_mangle]
extern "C" fn __dump_json(
    in_json_ptr: u64,
) -> /* arcstr_ptr */ u64 {
    // take back ownership
    let json = unsafe { Box::from_raw(in_json_ptr as *mut JsonFile) };

    // dump into an ArcStr
    let arcstr = json.dump(&JsonPath::new()).unwrap(/* panic = OOM */);
    let arcstr_ptr = ArcStr::into_raw(arcstr).as_ptr();

    arcstr_ptr as _
}

#[no_mangle]
extern "C" fn __json_dump_len(arcstr_ptr: u64) -> u64 {
    let arcstr = unsafe { ArcStr::from_raw(NonNull::new_unchecked(arcstr_ptr as _)) };
    let len = arcstr.len();
    core::mem::forget(arcstr);
    len as _
}

#[no_mangle]
extern "C" fn __json_dump_ptr(arcstr_ptr: u64) -> u64 {
    let arcstr = unsafe { ArcStr::from_raw(NonNull::new_unchecked(arcstr_ptr as _)) };
    let ptr = arcstr.as_str().as_ptr();
    core::mem::forget(arcstr);
    ptr as _
}

#[no_mangle]
extern "C" fn __free_json_dump(arcstr_ptr: u64) {
    unsafe { ArcStr::from_raw(NonNull::new_unchecked(arcstr_ptr as _)) };
}
