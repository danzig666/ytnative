# Appends this PC's dedicated ytnative public key to root's authorized_keys on a rooted LG TV.
# Touches only /home/root/.ssh (user data partition). Asks for the root password once.
# Usage: powershell -ExecutionPolicy Bypass -File tools\install-tv-key.ps1 [-TvHost 192.168.x.y]
param([string]$TvHost = $(if (Test-Path "$PSScriptRoot\..\.tv-host") { (Get-Content "$PSScriptRoot\..\.tv-host").Trim() } else { "" }))
if (-not $TvHost) { Write-Error "Pass -TvHost <ip> or put the TV address in .tv-host"; exit 1 }
$key = "$env:USERPROFILE\.ssh\id_lgtv_ytnative"
if (-not (Test-Path "$key.pub")) { ssh-keygen -t ed25519 -N '""' -C ytnative -f $key -q }
Get-Content "$key.pub" | ssh -o StrictHostKeyChecking=accept-new "root@$TvHost" 'mkdir -p ~/.ssh && chmod 700 ~/.ssh && cat >> ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys && echo KEY-INSTALLED'
