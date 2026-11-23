# Builds the downloadable Islet: dist\Islet-<version>-windows-x64.zip
#
# The release build replaces the builder's own folders (user folder, Rust's
# package cache, this project's folder) with neutral names, so no user name or
# path of this PC ends up inside the programs. The script then checks the two
# programs for the user name before packing them.
#
# Run from anywhere:  powershell -ExecutionPolicy Bypass -File tools\release.ps1

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$target = Join-Path $env:TEMP 'islet-public-build'
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { $env:PATH = (Join-Path $cargoHome 'bin') + ';' + $env:PATH }

# When several prefixes match, the last one given wins: most specific last.
$env:RUSTFLAGS = "--remap-path-prefix=$($env:USERPROFILE)=home --remap-path-prefix=$cargoHome=cargo --remap-path-prefix=$root=islet"
Write-Host "Building Islet $version ..."
cargo build --release --manifest-path (Join-Path $root 'Cargo.toml') --target-dir $target
if ($LASTEXITCODE -ne 0) { throw 'The build failed.' }
Remove-Item Env:RUSTFLAGS

$exes = 'islet.exe', 'islet-hook.exe' | ForEach-Object { Join-Path $target "release\$_" }

# Nothing about this PC inside: the user name, as plain and as wide text.
$name = $env:USERNAME
foreach ($exe in $exes) {
    $bytes = [IO.File]::ReadAllBytes($exe)
    foreach ($text in [Text.Encoding]::ASCII.GetString($bytes), [Text.Encoding]::Unicode.GetString($bytes)) {
        if ($text.IndexOf($name, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
            throw "$exe still contains the user name '$name'. Not packing it."
        }
    }
}
Write-Host "Checked: no '$name' inside the programs."

$dist = Join-Path $root 'dist'
$stage = Join-Path $dist 'Islet'
Remove-Item $stage -Recurse -ErrorAction SilentlyContinue
New-Item -ItemType Directory $stage -Force | Out-Null
Copy-Item $exes $stage
Copy-Item (Join-Path $root 'README.md'), (Join-Path $root 'LICENSE') $stage
$zip = Join-Path $dist "Islet-$version-windows-x64.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip
Remove-Item $stage -Recurse

$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
Write-Host ''
Write-Host "Ready: $zip"
Write-Host "Size:  $([math]::Round((Get-Item $zip).Length / 1MB, 2)) MB"
Write-Host "SHA-256: $hash"
