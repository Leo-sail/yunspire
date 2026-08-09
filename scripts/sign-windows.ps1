[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [ValidateSet('VerifyUnsignedInstaller')]
  [string]$Mode,

  [Parameter(Mandatory = $true)]
  [ValidatePattern('^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$')]
  [string]$ExpectedVersion,

  [Parameter(Mandatory = $true)]
  [ValidatePattern('^[0-9a-fA-F]{40}$')]
  [string]$ExpectedCommit,

  [Parameter(Mandatory = $true)]
  [ValidatePattern('^[0-9a-fA-F]{40}$')]
  [string]$ExpectedTree,

  [string]$TargetTriple = 'x86_64-pc-windows-msvc',

  [string]$GitHubOutput = $env:GITHUB_OUTPUT
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# This historical filename remains the Windows release entry point. The current
# v0.4.1 policy is intentionally unsigned, so this script verifies that state,
# the offline WebView2/current-user installer policy, and a real silent
# install/start cycle. It never signs an executable.

$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path -Path $PSScriptRoot -ChildPath '..'))
$releaseConfigPath = Join-Path -Path $repositoryRoot -ChildPath 'src-tauri/tauri.release.conf.json'

function Invoke-GitText {
  param(
    [Parameter(Mandatory = $true)]
    [string[]]$Arguments
  )

  $output = & git -C $repositoryRoot @Arguments 2>&1
  if ($LASTEXITCODE -ne 0) {
    throw "git $($Arguments -join ' ') failed: $($output -join [Environment]::NewLine)"
  }
  return ($output -join [Environment]::NewLine).Trim().ToLowerInvariant()
}

function Test-SourceIdentity {
  $sourceCommit = Invoke-GitText -Arguments @('rev-parse', '--verify', 'HEAD^{commit}')
  $sourceTree = Invoke-GitText -Arguments @('rev-parse', '--verify', 'HEAD^{tree}')
  if ($sourceCommit -ne $ExpectedCommit.ToLowerInvariant()) {
    throw "Checked-out commit $sourceCommit does not match $ExpectedCommit"
  }
  if ($sourceTree -ne $ExpectedTree.ToLowerInvariant()) {
    throw "Checked-out tree $sourceTree does not match $ExpectedTree"
  }
}

function Test-ReleaseInstallerConfiguration {
  if (-not (Test-Path -LiteralPath $releaseConfigPath -PathType Leaf)) {
    throw "Release configuration is missing: $releaseConfigPath"
  }
  $configuration = Get-Content -LiteralPath $releaseConfigPath -Raw | ConvertFrom-Json
  $bundle = $configuration.bundle
  $windows = $bundle.windows
  $nsis = $windows.nsis

  if ($null -ne $bundle.licenseFile) {
    throw 'Release installer must not add a separate license dialog'
  }
  if ($windows.allowDowngrades -ne $false) {
    throw 'Release installer must block downgrades'
  }
  if ($windows.webviewInstallMode.type -ne 'offlineInstaller' -or $windows.webviewInstallMode.silent -ne $true) {
    throw 'The complete WebView2 offline installer must be embedded and run silently'
  }
  $signCommandProperty = $windows.PSObject.Properties['signCommand']
  if ($null -ne $signCommandProperty -and $null -ne $signCommandProperty.Value) {
    throw 'Unsigned releases must not configure a Windows signing command'
  }
  if ($nsis.installMode -ne 'currentUser') {
    throw 'NSIS must use currentUser mode to avoid an elevation prompt'
  }
  if ($nsis.displayLanguageSelector -ne $false) {
    throw 'NSIS language selector must be disabled'
  }
  $languages = @($nsis.languages)
  if ($languages.Count -ne 2 -or $languages[0] -ne 'SimpChinese' -or $languages[1] -ne 'English') {
    throw 'NSIS languages must be exactly SimpChinese then English, with no selector'
  }
  foreach ($propertyName in @('template', 'installerHooks', 'startMenuFolder')) {
    $property = $nsis.PSObject.Properties[$propertyName]
    if ($null -ne $property -and $null -ne $property.Value) {
      throw "NSIS $propertyName must be unset to avoid custom or unnecessary installer pages"
    }
  }
}

function Get-UnsignedInstaller {
  $bundleDirectory = Join-Path -Path $repositoryRoot -ChildPath "src-tauri/target/$TargetTriple/release/bundle/nsis"
  $installers = @(Get-ChildItem -LiteralPath $bundleDirectory -Filter '*.exe' -File -ErrorAction Stop)
  if ($installers.Count -ne 1) {
    throw "Expected exactly one NSIS installer in $bundleDirectory; found $($installers.Count)"
  }
  if ($installers[0].Length -lt 100MB) {
    throw "NSIS installer is too small to contain the complete offline WebView2 runtime: $($installers[0].Length) bytes"
  }
  return $installers[0].FullName
}

function Test-UnsignedExecutable {
  param(
    [Parameter(Mandatory = $true)]
    [string]$LiteralPath,

    [Parameter(Mandatory = $true)]
    [string]$Label
  )

  if (-not (Test-Path -LiteralPath $LiteralPath -PathType Leaf)) {
    throw "$Label is missing: $LiteralPath"
  }
  $signature = Get-AuthenticodeSignature -LiteralPath $LiteralPath
  if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::NotSigned) {
    throw "$Label must be explicitly unsigned for this release; status is $($signature.Status): $LiteralPath"
  }
}

function Set-ActionOutput {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Name,

    [Parameter(Mandatory = $true)]
    [string]$Value
  )

  if ([string]::IsNullOrWhiteSpace($GitHubOutput)) {
    return
  }
  Add-Content -LiteralPath $GitHubOutput -Value "$Name=$Value" -Encoding utf8
}

function Invoke-UnsignedInstallerSmokeTest {
  param(
    [Parameter(Mandatory = $true)]
    [string]$InstallerPath
  )

  $temporaryRoot = if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
    [System.IO.Path]::GetTempPath()
  } else {
    $env:RUNNER_TEMP
  }
  $installDirectory = Join-Path -Path $temporaryRoot -ChildPath "yunspire-install-smoke-$([Guid]::NewGuid().ToString('N'))"
  $currentUserUninstallKey = 'Registry::HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall\Yunspire'
  $machineUninstallKey = 'Registry::HKEY_LOCAL_MACHINE\Software\Microsoft\Windows\CurrentVersion\Uninstall\Yunspire'
  $applicationProcess = $null

  try {
    $installProcess = Start-Process -FilePath $InstallerPath `
      -ArgumentList @('/S', "/D=$installDirectory") `
      -Wait `
      -PassThru `
      -WindowStyle Hidden
    if ($installProcess.ExitCode -ne 0) {
      throw "NSIS silent installation exited with code $($installProcess.ExitCode)"
    }

    if (-not (Test-Path -LiteralPath $currentUserUninstallKey)) {
      throw 'Current-user NSIS installation did not register under HKCU'
    }
    if (Test-Path -LiteralPath $machineUninstallKey) {
      throw 'Current-user NSIS installation unexpectedly registered under HKLM'
    }
    $registration = Get-ItemProperty -LiteralPath $currentUserUninstallKey
    $registeredDirectory = ([string]$registration.InstallLocation).Trim([char]0x22)
    if ([System.IO.Path]::GetFullPath($registeredDirectory) -ne [System.IO.Path]::GetFullPath($installDirectory)) {
      throw "HKCU install location $registeredDirectory does not match $installDirectory"
    }
    if ([string]$registration.DisplayVersion -ne $ExpectedVersion) {
      throw "HKCU product version $($registration.DisplayVersion) does not match $ExpectedVersion"
    }

    $application = Get-ChildItem -LiteralPath $installDirectory -Filter 'Yunspire.exe' -File -Recurse |
      Select-Object -First 1
    if ($null -eq $application) {
      throw "Installed Yunspire.exe was not found under $installDirectory"
    }
    Test-UnsignedExecutable -LiteralPath $application.FullName -Label 'Installed Yunspire application'
    if (-not $application.VersionInfo.ProductVersion.StartsWith($ExpectedVersion, [StringComparison]::Ordinal)) {
      throw "Installed product version $($application.VersionInfo.ProductVersion) does not match $ExpectedVersion"
    }

    foreach ($relativePath in @(
      'skills/document-content-analysis/scripts/yunspire_pdf_windows.exe',
      'skills/document-content-analysis/scripts/yunspire_image_windows.exe',
      'skills/video-content-analysis/scripts/bin/yunspire-media.exe',
      'skills/video-content-analysis/scripts/bin/yunspire-speech.exe'
    )) {
      $helperPath = Join-Path -Path $installDirectory -ChildPath $relativePath
      Test-UnsignedExecutable -LiteralPath $helperPath -Label "Installed helper $relativePath"
    }

    $privacyVerifier = Join-Path -Path $repositoryRoot -ChildPath 'scripts/verify-packaged-privacy.mjs'
    & node $privacyVerifier --directory $installDirectory --platform windows
    if ($LASTEXITCODE -ne 0) {
      throw "Installed package privacy verification failed with exit code $LASTEXITCODE"
    }

    $applicationProcess = Start-Process -FilePath $application.FullName -PassThru
    $startupDeadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
      Start-Sleep -Milliseconds 500
      $applicationProcess.Refresh()
      if ($applicationProcess.HasExited) {
        throw "Installed Yunspire exited during startup with code $($applicationProcess.ExitCode)"
      }
    } while ($applicationProcess.MainWindowHandle -eq 0 -and [DateTime]::UtcNow -lt $startupDeadline)
    if ($applicationProcess.MainWindowHandle -eq 0) {
      throw 'Installed Yunspire stayed alive but did not create an application window within 30 seconds'
    }
  } finally {
    if ($null -ne $applicationProcess -and -not $applicationProcess.HasExited) {
      Stop-Process -Id $applicationProcess.Id -Force -ErrorAction SilentlyContinue
      $applicationProcess.WaitForExit(15000)
    }

    if (Test-Path -LiteralPath $installDirectory -PathType Container) {
      $uninstaller = Get-ChildItem -LiteralPath $installDirectory -Filter 'uninstall*.exe' -File -ErrorAction SilentlyContinue |
        Select-Object -First 1
      if ($null -ne $uninstaller) {
        Test-UnsignedExecutable -LiteralPath $uninstaller.FullName -Label 'Installed uninstaller'
        $uninstallProcess = Start-Process -FilePath $uninstaller.FullName `
          -ArgumentList '/S' `
          -Wait `
          -PassThru `
          -WindowStyle Hidden
        if ($uninstallProcess.ExitCode -ne 0) {
          throw "NSIS silent uninstall exited with code $($uninstallProcess.ExitCode)"
        }
      }
      Remove-Item -LiteralPath $installDirectory -Recurse -Force -ErrorAction SilentlyContinue
    }
  }
}

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
  throw 'Windows release verification must run on Windows'
}
if ($Mode -ne 'VerifyUnsignedInstaller') {
  throw "Unsupported mode: $Mode"
}

Test-SourceIdentity
Test-ReleaseInstallerConfiguration
$installer = Get-UnsignedInstaller
Test-UnsignedExecutable -LiteralPath $installer -Label 'NSIS installer'
Invoke-UnsignedInstallerSmokeTest -InstallerPath $installer
Set-ActionOutput -Name 'installer' -Value $installer
Write-Output "WINDOWS_UNSIGNED_INSTALLER_OK version=$ExpectedVersion commit=$($ExpectedCommit.ToLowerInvariant()) installer=$installer"
