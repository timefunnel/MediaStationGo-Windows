param([Parameter(Mandatory=$true)][string]$MediaId, [switch]$MetadataOnly,
      [string]$ApplicationDirectory = '', [string]$VfgRuntime = '')
$ErrorActionPreference='Stop'
$RepoRoot=(Get-Item $PSScriptRoot).Parent.Parent.FullName
. (Join-Path $PSScriptRoot 'env.ps1')
Add-Type -TypeDefinition @'
using System;using System.Runtime.InteropServices;public static class MovieHdrSession{[StructLayout(LayoutKind.Sequential)]public struct C{public uint F,T;public IntPtr N,M;public long W;public uint S;public IntPtr B;public uint P,Q;public IntPtr A,L,U;}[DllImport("advapi32.dll",CharSet=CharSet.Unicode)]static extern bool CredReadW(string n,uint t,uint f,out IntPtr p);[DllImport("advapi32.dll")]static extern void CredFree(IntPtr p);public static string Read(){IntPtr p;if(!CredReadW("MediaStationGo.Windows.ActiveSession.v1",1,0,out p))throw new Exception("No active session");try{var c=Marshal.PtrToStructure<C>(p);var b=new byte[c.S];Marshal.Copy(c.B,b,0,b.Length);return System.Text.Encoding.UTF8.GetString(b);}finally{CredFree(p);}}}
'@
$Session=[MovieHdrSession]::Read()|ConvertFrom-Json
$Base=[Uri]$Session.baseUrl
$Handler=[Net.Http.HttpClientHandler]::new();$Handler.AllowAutoRedirect=$false;$Handler.UseProxy=$false
$Client=[Net.Http.HttpClient]::new($Handler);$Client.Timeout=[TimeSpan]::FromSeconds(20)
try{
    $Url=$Session.baseUrl.TrimEnd('/')+'/Items/'+$MediaId+'/PlaybackInfo?UserId='+$Session.userId+'&AutoOpenLiveStream=false&IsPlayback=true'
    $Request=[Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::Get,$Url)
    [void]$Request.Headers.TryAddWithoutValidation('X-Emby-Token',$Session.accessToken)
    [void]$Request.Headers.TryAddWithoutValidation('User-Agent','MediaStationGoWindows/1.1.0')
    $Response=$Client.Send($Request)
    if([int]$Response.StatusCode -ne 200){throw ('PlaybackInfo HTTP '+[int]$Response.StatusCode)}
    $Info=$Response.Content.ReadAsStringAsync().GetAwaiter().GetResult()|ConvertFrom-Json
    $Response.Dispose();$Request.Dispose()
    $Source=$Info.MediaSources[0]
    if(-not $Source.DirectStreamUrl){throw 'No direct stream URL in current PlaybackInfo'}
    $Resource=[Uri]::new($Base,[string]$Source.DirectStreamUrl)
    $Resolved=$false
    for($Hop=0;$Hop -lt 10;$Hop++){
        $Request=[Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::Head,$Resource)
        [void]$Request.Headers.TryAddWithoutValidation('User-Agent','MediaStationGoWindows/1.1.0')
        if($Resource.GetLeftPart([UriPartial]::Authority) -eq $Base.GetLeftPart([UriPartial]::Authority)){
            [void]$Request.Headers.TryAddWithoutValidation('X-Emby-Token',$Session.accessToken)
        }
        $Response=$Client.Send($Request)
        $Status=[int]$Response.StatusCode
        if($Status -in 301,302,303,307,308){$Resource=[Uri]::new($Resource,$Response.Headers.Location);$Response.Dispose();$Request.Dispose();continue}
        $Response.Dispose();$Request.Dispose()
        if($Status -ne 200){throw ('Media HEAD HTTP '+$Status)}
        $Resolved=$true;break
    }
    if(-not $Resolved){throw 'Media redirect limit reached'}
    if($Resource.GetLeftPart([UriPartial]::Authority) -eq $Base.GetLeftPart([UriPartial]::Authority)){throw 'Media remained on authenticated server origin; direct probe not attempted'}
    $Start=[Diagnostics.ProcessStartInfo]::new('C:\msys64\clang64\bin\ffprobe.exe')
    $Start.UseShellExecute=$false;$Start.CreateNoWindow=$true;$Start.RedirectStandardOutput=$true;$Start.RedirectStandardError=$true
    foreach($Argument in @('-v','error','-user_agent','MediaStationGoWindows/1.1.0','-rw_timeout','15000000','-analyzeduration','10000000','-probesize','16000000','-select_streams','v:0','-read_intervals','%+0.1','-show_streams','-show_frames','-of','json',$Resource.AbsoluteUri)){[void]$Start.ArgumentList.Add($Argument)}
    $Probe=[Diagnostics.Process]::Start($Start)
    $OutputTask=$Probe.StandardOutput.ReadToEndAsync();$ErrorTask=$Probe.StandardError.ReadToEndAsync()
    if(-not $Probe.WaitForExit(60000)){$Probe.Kill($true);throw 'Real-source metadata probe timed out'}
    if($Probe.ExitCode -ne 0){throw ('Real-source ffprobe failed with exit '+$Probe.ExitCode)}
    $Data=$OutputTask.GetAwaiter().GetResult()|ConvertFrom-Json
    if(-not $Data.frames.Count){throw 'Real-source metadata probe returned no decoded video frames'}
    $Video=$Data.streams[0]
    $Dovi=$Data.frames[0].side_data_list|Where-Object {$_.side_data_type -eq 'Dolby Vision Metadata'}
    $Safe=[pscustomobject]@{MediaId=$MediaId;Codec=$Video.codec_name;Width=$Video.width;Height=$Video.height;DolbyConfiguration=@($Video.side_data_list|Where-Object {$_.side_data_type -like '*DOVI*'});FrameSideData=@($Data.frames[0].side_data_list.side_data_type);RpuHeaders=@($Data.frames[0].side_data_list|Where-Object {$_.side_data_type -eq 'Dolby Vision Metadata'}|Select-Object rpu_type,rpu_format,vdr_rpu_profile,disable_residual_flag)}
    $Safe|Add-Member -NotePropertyName RpuMapping -NotePropertyValue ($Dovi|Select-Object coef_log2_denom,nlq_method_idc,nlq_method_idc_name,bl_bit_depth,el_bit_depth,vdr_bit_depth,components)
    $Json=$Safe|ConvertTo-Json -Depth 16
    [IO.File]::WriteAllText((Join-Path $RepoRoot 'build/video-enhancement/dynamic-hdr-real-source-metadata.json'),$Json,[Text.UTF8Encoding]::new($false))
    Write-Output $Json
    if(-not $MetadataOnly) {
        $Mpv=Join-Path $RepoRoot 'third_party/mpv-install'
        $Native=Join-Path $RepoRoot 'build/video-enhancement'
        $RuntimeLibraries=Join-Path $Mpv 'lib'
        $Bridge=Join-Path $Native 'video_enhancement.dll'
        if($ApplicationDirectory) {
            $RuntimeLibraries=$ApplicationDirectory
            if(-not (Test-Path -LiteralPath (Join-Path $RuntimeLibraries 'libmpv-2.dll') -PathType Leaf)){throw 'Application directory is missing libmpv-2.dll'}
            $Bridge=Join-Path $ApplicationDirectory 'video_enhancement.dll'
        }
        $Sdk=Join-Path $RepoRoot 'third_party/video-enhancement-deps/RTX-Video-SDK-1.1.0'
        $Veyra=Join-Path $RepoRoot 'third_party/video-enhancement-deps/Veyra-2.0.6-NVIDIA/runtime'
        if(-not $VfgRuntime){$VfgRuntime=Join-Path $Veyra 'nvidia-vfg'}
        $TestRoot=Join-Path $RepoRoot ('build/dynamic-hdr-live-'+[guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $TestRoot|Out-Null
        try {
            & cl.exe /nologo /MD /std:c++17 /EHsc /W4 /WX "/I$Mpv/include" (Join-Path $PSScriptRoot 'mpv/test_dynamic_hdr_playback.cpp') "/Fo$TestRoot/probe.obj" "/Fe$TestRoot/probe.exe" /link "$Mpv/lib/mpv.lib" user32.lib
            if($LASTEXITCODE -ne 0){throw 'Live HDR probe compilation failed'}
            $Start=[Diagnostics.ProcessStartInfo]::new((Join-Path $TestRoot 'probe.exe'))
            $Start.UseShellExecute=$false;$Start.CreateNoWindow=$true
            $Start.RedirectStandardInput=$true;$Start.RedirectStandardOutput=$true;$Start.RedirectStandardError=$true
            $Start.Environment['PATH']=$RuntimeLibraries+';'+$env:PATH
            foreach($Argument in @('unused','unused','unused',$Bridge,"$Sdk/bin/Windows/x64/rel",$VfgRuntime,$TestRoot,"$Veyra/experimental/nr-original",'live')){[void]$Start.ArgumentList.Add($Argument)}
            $Playback=[Diagnostics.Process]::Start($Start)
            $OutTask=$Playback.StandardOutput.ReadToEndAsync();$ErrTask=$Playback.StandardError.ReadToEndAsync()
            # The signed URL stays in memory and goes only through stdin. Native
            # mpv logs are suppressed in live mode; no session/progress is posted.
            $Playback.StandardInput.WriteLine($Resource.AbsoluteUri);$Playback.StandardInput.Close()
            if(-not $Playback.WaitForExit(590000)){$Playback.Kill($true);throw 'Live HDR playback test timed out'}
            $SafeOutput=$OutTask.GetAwaiter().GetResult()+$ErrTask.GetAwaiter().GetResult()
            [IO.File]::WriteAllText((Join-Path $Native 'dynamic-hdr-real-playback.log'),$SafeOutput,[Text.UTF8Encoding]::new($false))
            Write-Output $SafeOutput
            if($Playback.ExitCode -ne 0){throw ('Live HDR playback probe failed with exit '+$Playback.ExitCode)}
        } finally {
            if($Playback -and -not $Playback.HasExited){$Playback.Kill($true);$Playback.WaitForExit()}
            $Absolute=[IO.Path]::GetFullPath($TestRoot)
            $Allowed=[IO.Path]::GetFullPath((Join-Path $RepoRoot 'build')).TrimEnd('\')+'\'
            if(-not $Absolute.StartsWith($Allowed,[StringComparison]::OrdinalIgnoreCase)){throw 'Unexpected live test cleanup target'}
            Remove-Item -LiteralPath $Absolute -Recurse -Force
        }
    }
}finally{$Client.Dispose();$Session=$null}
