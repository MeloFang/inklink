//! 诊断工具：查 Windows 是否真的认识我们的 TIP。

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize,
};
use windows::Win32::UI::TextServices::{
    CLSID_TF_InputProcessorProfiles, ITfInputProcessorProfiles, ITfTextInputProcessorEx,
    TF_LANGUAGEPROFILE,
};
use windows::core::GUID;

const CLSID_INKLINK: GUID = GUID::from_u128(0x8f2a6c1e_4b3d_4f7a_9a21_5c6d7e8f9a0b);
const LANG_ZH_CN: u16 = 0x0804;

fn main() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        println!("=== 1. enumerate 0x804 language profiles ===");
        if let Ok(profiles) = CoCreateInstance::<_, ITfInputProcessorProfiles>(
            &CLSID_TF_InputProcessorProfiles,
            None,
            CLSCTX_INPROC_SERVER,
        ) {
            match profiles.EnumLanguageProfiles(LANG_ZH_CN) {
                Ok(en) => {
                    let mut buf = [TF_LANGUAGEPROFILE::default()];
                    loop {
                        let mut fetched = 0u32;
                        if en.Next(&mut buf, &mut fetched).is_err() || fetched == 0 {
                            break;
                        }
                        let p = &buf[0];
                        println!(
                            "  clsid={:?} profile={:?} active={}",
                            p.clsid,
                            p.guidProfile,
                            p.fActive.as_bool()
                        );
                    }
                }
                Err(e) => println!("  EnumLanguageProfiles failed: {:?}", e),
            }
        } else {
            println!("  cannot get ITfInputProcessorProfiles");
        }

        println!("=== 2. instantiate our TIP ===");
        match CoCreateInstance::<_, ITfTextInputProcessorEx>(
            &CLSID_INKLINK,
            None,
            CLSCTX_INPROC_SERVER,
        ) {
            Ok(_tip) => println!("  CoCreateInstance ITfTextInputProcessorEx: OK"),
            Err(e) => println!("  CoCreateInstance failed: {:?}", e),
        }

        CoUninitialize();
    }
}
