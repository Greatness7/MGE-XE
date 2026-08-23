use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::process::ExitCode;

unsafe extern "C" {
    fn mge_dl_contract_main(argc: c_int, argv: *const *const c_char) -> c_int;
}

fn main() -> ExitCode {
    let args: Vec<CString> = std::env::args()
        .map(|a| CString::new(a).expect("argument contains interior NUL"))
        .collect();
    let argv: Vec<*const c_char> = args.iter().map(|a| a.as_ptr()).collect();

    let code = unsafe { mge_dl_contract_main(argv.len() as c_int, argv.as_ptr()) };
    ExitCode::from(code as u8)
}
