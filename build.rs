use std::{env, fs, path::PathBuf, process::Command};
fn main() {
    println!("cargo:rerun-if-changed=assets/guangxia.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-env-changed=RC_EXE");
    if env::var("CARGO_CFG_TARGET_OS").as_deref()!=Ok("windows") {return;}
    let root=PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out=PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let icon=root.join("assets/guangxia.ico").to_string_lossy().replace('\\',"/");
    let version=env::var("CARGO_PKG_VERSION").unwrap();
    let numbers=format!("{},0",version.replace('.',","));
    let resource=format!(r#"#pragma code_page(65001)
1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {numbers}
PRODUCTVERSION {numbers}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
 BLOCK "StringFileInfo"
 BEGIN
  BLOCK "080404B0"
  BEGIN
   VALUE "FileDescription", "光匣 · 本地影像管家\0"
   VALUE "FileVersion", "{version}\0"
   VALUE "InternalName", "Guangxia\0"
   VALUE "OriginalFilename", "Guangxia.exe\0"
   VALUE "ProductName", "光匣\0"
   VALUE "ProductVersion", "{version}\0"
  END
 END
 BLOCK "VarFileInfo"
 BEGIN
  VALUE "Translation", 0x804, 1200
 END
END
"#);
    let rc=out.join("guangxia.rc"); let res=out.join("guangxia.res");fs::write(&rc,resource).unwrap();
    let compiler=env::var_os("RC_EXE").map(PathBuf::from).or_else(|| {
        let base=PathBuf::from(env::var_os("ProgramFiles(x86)")?).join("Windows Kits/10/bin");
        let mut dirs:Vec<_>=fs::read_dir(base).ok()?.filter_map(Result::ok).map(|e|e.path().join("x64/rc.exe")).filter(|p|p.is_file()).collect();
        dirs.sort();dirs.pop()
    }).unwrap_or_else(||PathBuf::from("rc.exe"));
    let status=Command::new(&compiler).arg("/nologo").arg("/fo").arg(&res).arg(&rc).status().expect("需要 Windows SDK 的 rc.exe（或设置 RC_EXE）来嵌入光匣图标");
    assert!(status.success(),"编译光匣图标资源失败");
    println!("cargo:rustc-link-arg-bin=photo-video-manager={}",res.display());
}
