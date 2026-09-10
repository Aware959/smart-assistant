#Requires -Version 7.0
<#
.SYNOPSIS
    生成自签名 TLS 证书，供后端 HTTPS 与本地/局域网浏览器访问。

.DESCRIPTION
    使用 Windows 内置 .NET (CertificateRequest) 生成，无需 openssl。
    SAN 覆盖 localhost、本机名、127.0.0.1、::1 以及所有活动网卡的局域网 IPv4，
    方便手机通过 https://<局域网IP>:3000 访问。

.NOTES
    输出到 backend/certs/：server.pem（证书含 serverAuth EKU）与 server.key（PKCS#8）。
    该目录已加入 .gitignore，不会提交。
#>
[CmdletBinding()]
param(
    [string]$Directory = (Join-Path (Split-Path $PSScriptRoot -Parent) 'certs'),
    [int]$Days = 3650
)

function Get-LocalIPv4Addresses {
    [System.Net.NetworkInformation.NetworkInterface]::GetAllNetworkInterfaces() |
        Where-Object { $_.OperationalStatus -eq 'Up' -and $_.NetworkInterfaceType -ne 'Loopback' } |
        ForEach-Object { $_.GetIPProperties().UnicastAddresses } |
        Where-Object { $_.Address.AddressFamily -eq 'InterNetwork' } |
        ForEach-Object { $_.Address.ToString() } |
        Sort-Object -Unique
}

New-Item -ItemType Directory -Path $Directory -Force | Out-Null

$rsa = [System.Security.Cryptography.RSA]::Create(3072)
$req = [System.Security.Cryptography.X509Certificates.CertificateRequest]::new(
    'CN=Smart Assistant Dev',
    $rsa,
    [System.Security.Cryptography.HashAlgorithmName]::SHA256,
    [System.Security.Cryptography.RSASignaturePadding]::Pkcs1)

$san = [System.Security.Cryptography.X509Certificates.SubjectAlternativeNameBuilder]::new()
$san.AddDnsName('localhost')
$san.AddDnsName([System.Net.Dns]::GetHostName())
$san.AddIpAddress([System.Net.IPAddress]::Parse('127.0.0.1'))
$san.AddIpAddress([System.Net.IPAddress]::Parse('::1'))
foreach ($ip in Get-LocalIPv4Addresses) {
    $san.AddIpAddress([System.Net.IPAddress]::Parse($ip))
}
$req.CertificateExtensions.Add($san.Build())

$serverAuthOids = [System.Security.Cryptography.OidCollection]::new()
$serverAuthOids.Add([System.Security.Cryptography.Oid]::new('1.3.6.1.5.5.7.3.1'))
$req.CertificateExtensions.Add(
    [System.Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension]::new(
        $serverAuthOids, $false))

$req.CertificateExtensions.Add(
    [System.Security.Cryptography.X509Certificates.X509KeyUsageExtension]::new(
        [System.Security.Cryptography.X509Certificates.X509KeyUsageFlags]::DigitalSignature -bor
            [System.Security.Cryptography.X509Certificates.X509KeyUsageFlags]::KeyEncipherment,
        $false))

$cert = $req.CreateSelfSigned(
    [DateTimeOffset]::Now.AddDays(-2),
    [DateTimeOffset]::Now.AddDays($Days))

$certPath = Join-Path $Directory 'server.pem'
$keyPath = Join-Path $Directory 'server.key'
[System.IO.File]::WriteAllText($certPath, $cert.ExportCertificatePem())
[System.IO.File]::WriteAllText($keyPath, $rsa.ExportPkcs8PrivateKeyPem())

Write-Host '证书已生成:'
Write-Host "  cert: $certPath"
Write-Host "  key:  $keyPath"
Write-Host ''
Write-Host "SAN: localhost, $([System.Net.Dns]::GetHostName()), 127.0.0.1, ::1 + 本机局域网 IP"
Write-Host ''
Write-Host '浏览器访问 https://localhost:3000 提示不受信任时：'
Write-Host '  - 桌面 Chrome：打开页面后输入 thisisunsafe 继续；Firefox：添加例外'
Write-Host '  - 手机（iOS/Android）：安装 server.pem 并信任后，可访问 https://<局域网IP>:3000'