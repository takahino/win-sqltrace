<#
.SYNOPSIS
    コミット前とレビュー前の確認: 整形、clippy、試験、OS 非依存の crate の Linux 向け check。
.DESCRIPTION
    必要なもの: rust-toolchain.toml の toolchain（rustup が自動で入れる）と MSVC のリンカー。
    どれかが失敗したら、そこで止めて終了コード 1 を返す。
.PARAMETER Fix
    整形を確認するだけでなく、その場で直す（cargo fmt）。
#>
[CmdletBinding()]
param(
    [switch]$Fix
)

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
# 確認は毎回まとめてビルドするので増分ビルドは要らない。P: ドライブなどで増分の一時ファイルの移動が
# 「アクセスが拒否されました」で失敗することがあり、ハードリンクの警告も出るので切る。
$env:CARGO_INCREMENTAL = '0'

$windowsTargets = @('x86_64-pc-windows-msvc', 'i686-pc-windows-msvc')
# Windows の API に触れない crate。ここに Linux 向けの check を通し、OS 依存が混ざっていないことを確かめる。
# OS 非依存の crate を足したら、ここにも足す。
$portableCrates = @('sqltrace-core', 'sqltrace-catalog', 'sqltrace-export', 'sqltrace-build')

function Invoke-Step([string]$name, [scriptblock]$command) {
    Write-Host "==> $name" -ForegroundColor Cyan
    & $command
    if ($LASTEXITCODE -ne 0) {
        Write-Host "失敗: $name (終了コード $LASTEXITCODE)。上の出力を見て直してから、もう一度実行してください。" -ForegroundColor Red
        exit 1
    }
}

if ($Fix) {
    Invoke-Step 'cargo fmt' { cargo fmt --all }
} else {
    Invoke-Step 'cargo fmt --check' { cargo fmt --all -- --check }
}

foreach ($target in $windowsTargets) {
    Invoke-Step "clippy ($target)" { cargo clippy --workspace --all-targets --target $target -- -D warnings }
    Invoke-Step "test ($target)" { cargo test --workspace --target $target }
}

$packages = $portableCrates | ForEach-Object { '-p', $_ }
Invoke-Step 'check (x86_64-unknown-linux-gnu, OS 非依存の crate)' {
    cargo check @packages --target x86_64-unknown-linux-gnu
}

Write-Host 'すべて通りました。' -ForegroundColor Green
