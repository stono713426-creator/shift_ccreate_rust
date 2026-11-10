//! Windows 実行ファイル (.exe) へのバージョン情報リソース自動埋め込みスクリプト
//!
//! Version: v1.3.2
//! Updated: 2026-11-10 - v1.3.2 リリースバージョン同期
//! - v1.3.0 (2026-11-08): リリースバージョン同期
//! - v1.2.1 (2026-11-05): windres プリプロセッサに LLVM clang.exe フォールバックを追加（Windows Smart App Control 対応）
//! - v1.0.0 (2026-10-31): Cargo.toml の version から自動的に FileVersion / ProductVersion を埋め込む初期構築

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let pkg_version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "1.0.0".to_string());
    let parts: Vec<u16> = pkg_version
        .split('.')
        .map(|s| s.parse::<u16>().unwrap_or(0))
        .collect();
    let major = *parts.first().unwrap_or(&1);
    let minor = *parts.get(1).unwrap_or(&0);
    let patch = *parts.get(2).unwrap_or(&0);
    let ver_str = format!("v{major}.{minor}.{patch}");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
    let rc_path = out_dir.join("version_auto.rc");
    let obj_path = out_dir.join("version_auto.o");

    let rc_content = format!(
        r#"1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "041104b0"
        BEGIN
            VALUE "FileDescription", "勤務シフト自動生成システム (Rust Edition)\0"
            VALUE "FileVersion", "{ver_str}\0"
            VALUE "ProductName", "shift_create_rust\0"
            VALUE "ProductVersion", "{ver_str}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0411, 1200
    END
END
"#
    );

    fs::write(&rc_path, rc_content).expect("Failed to write version_auto.rc");

    let windres_candidates = [
        "windres.exe",
        r"C:\Users\stono\AppData\Local\Microsoft\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT_Microsoft.Winget.Source_8wekyb3d8bbwe\mingw64\bin\windres.exe",
        r"C:\Users\stono\AppData\Local\Programs\mingw64\bin\windres.exe",
    ];

    let clang_pp = r"C:\Program Files\LLVM\bin\clang.exe";

    for bin in windres_candidates {
        if let Ok(status) = Command::new(bin)
            .args([
                "-c",
                "65001",
                rc_path.to_str().unwrap(),
                "-O",
                "coff",
                "-o",
                obj_path.to_str().unwrap(),
            ])
            .status()
        {
            if status.success() {
                println!("cargo:rustc-link-arg-bins={}", obj_path.display());
                return;
            }
        }

        if PathBuf::from(clang_pp).exists() {
            if let Ok(status) = Command::new(bin)
                .args([
                    &format!("--preprocessor={clang_pp}"),
                    "--preprocessor-arg=-E",
                    "--preprocessor-arg=-xc-header",
                    "--preprocessor-arg=-DRC_INVOKED",
                    "-c",
                    "65001",
                    rc_path.to_str().unwrap(),
                    "-O",
                    "coff",
                    "-o",
                    obj_path.to_str().unwrap(),
                ])
                .status()
            {
                if status.success() {
                    println!("cargo:rustc-link-arg-bins={}", obj_path.display());
                    return;
                }
            }
        }
    }
}
