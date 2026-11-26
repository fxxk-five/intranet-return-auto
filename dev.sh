#!/usr/bin/env bash
# 本机开发用：把工程外置的 Rust 工具链 + mingw-w64 挂上 PATH 后执行 cargo。
# 交付物不依赖本脚本（内网机只用 probe.exe）。
#
# 三个环境细节都是踩过的坑，别改：
#  1) 工具链与 build 目录必须是纯 ASCII 路径（这里用 D:\rt 和 D:\probe-target）。
#     GNU ld / dlltool 用系统 ANSI 代码页接收 argv，路径含中文（本工程目录
#     "内网退货自动处理程序"）时 ld 会报 "cannot find ...rlib: No such file or directory"。
#  2) 必须有 dlltool.exe（mingw-w64 binutils），否则 windows crate 的
#     windows_x86_64_gnu 构建脚本会 error calling dlltool。
#  3) PATH 用 Windows 形式（反斜杠 + 分号），用 /d/... 形式时 Rust 的
#     Command 找不到 rustc/dlltool（CreateProcess 不认 MSYS 路径）。
set -e
cd "$(dirname "$0")"
export PATH='D:\rt\rust\bin;D:\rt\mingw64\bin;C:\Windows\System32;C:\Windows'
export CARGO_HOME='D:\rt\cargo-home'
export CARGO_TARGET_DIR='D:\probe-target'
cd probe
exec cargo "$@"
