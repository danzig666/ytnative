# Fetches the Windows simulator's third-party libraries into third_party/win/ (gitignored):
# the official SDL2 and SDL2_ttf "VC" development zips from github.com/libsdl-org releases.
# Every download is checked against the SHA-256 pinned below before it is unpacked, and the
# hash is printed. Nothing here is committed: the zips and their DLLs/import libs stay local.
#
#   powershell -ExecutionPolicy Bypass -File tools/fetch-sim-deps.ps1
#
# Pins: SDL2's hash matches the "digest" GitHub publishes for the release asset; SDL_ttf's
# release predates GitHub asset digests, so its hash was recorded from the first download.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root 'third_party\win'
New-Item -ItemType Directory -Force $dest | Out-Null

$deps = @(
    @{
        Name = 'SDL2-2.32.10'
        Url  = 'https://github.com/libsdl-org/SDL/releases/download/release-2.32.10/SDL2-devel-2.32.10-VC.zip'
        Sha  = 'af347939395a58b365846aaea27391e69f9ec9d4dd650d6ac40802159b418a6e'
    },
    @{
        Name = 'SDL2_ttf-2.24.0'
        Url  = 'https://github.com/libsdl-org/SDL_ttf/releases/download/release-2.24.0/SDL2_ttf-devel-2.24.0-VC.zip'
        Sha  = '2dea8ea01e04756ead27e196a681034ed71342562a75b512ea279fcdfde83307'
    }
)

foreach ($d in $deps) {
    $dir = Join-Path $dest $d.Name
    if (Test-Path (Join-Path $dir 'lib\x64')) {
        Write-Host "$($d.Name): present"
        continue
    }
    $zip = Join-Path $dest ($d.Name + '.zip')
    Write-Host "$($d.Name): downloading $($d.Url)"
    Invoke-WebRequest -Uri $d.Url -OutFile $zip -UseBasicParsing
    $sha = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
    Write-Host "$($d.Name): sha256 $sha"
    if ($sha -ne $d.Sha) {
        Remove-Item $zip
        throw "$($d.Name): SHA-256 mismatch (expected $($d.Sha))"
    }
    Expand-Archive -Path $zip -DestinationPath $dest -Force
    Remove-Item $zip
    if (-not (Test-Path (Join-Path $dir 'lib\x64'))) {
        throw "$($d.Name): unexpected archive layout (no lib\x64 under $dir)"
    }
    Write-Host "$($d.Name): unpacked to $dir"
}
Write-Host 'third_party/win ready'
