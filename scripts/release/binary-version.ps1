# Direct, bounded version probes. Child output bodies never become diagnostics.
function Get-AurumBinaryVersion {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][ValidateSet('aurum','aurum-studio')][string]$Prefix
    )
    $resolved = (Resolve-Path -LiteralPath $Path).Path
    $start = [Diagnostics.ProcessStartInfo]::new($resolved)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.ArgumentList.Add('--version')
    $start.WorkingDirectory = Split-Path $resolved -Parent
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $process = $null
    $failure = 'version_probe_start_failed'
    $success = $false
    try {
        $process = [Diagnostics.Process]::Start($start)
        $output = [Collections.Generic.List[byte]]::new()
        $buffers = @([byte[]]::new(1024),[byte[]]::new(1024))
        $streams = @($process.StandardOutput.BaseStream,$process.StandardError.BaseStream)
        $reads = @($streams[0].ReadAsync($buffers[0],0,1024),$streams[1].ReadAsync($buffers[1],0,1024))
        $finished = @($false,$false)
        $stderrBytes = 0
        while ($timer.Elapsed.TotalSeconds -lt 3) {
            for ($index=0; $index -lt 2; $index++) {
                if (-not $finished[$index] -and $reads[$index].IsCompleted) {
                    $length = $reads[$index].GetAwaiter().GetResult()
                    if ($length -eq 0) { $finished[$index] = $true; continue }
                    if ($index -eq 0) {
                        if ($output.Count + $length -gt 256) { $failure='version_probe_output_limit'; throw $failure }
                        for ($offset=0; $offset -lt $length; $offset++) { $output.Add($buffers[$index][$offset]) }
                    } else {
                        $stderrBytes += $length
                        if ($stderrBytes -gt 4096) { $failure='version_probe_stderr_limit'; throw $failure }
                    }
                    $reads[$index] = $streams[$index].ReadAsync($buffers[$index],0,1024)
                }
            }
            if ($process.HasExited -and $finished[0] -and $finished[1]) {
                $failure = 'version_probe_exit_failed'
                if ($process.ExitCode -ne 0) { throw $failure }
                $text = [Text.Encoding]::UTF8.GetString($output.ToArray()).Trim()
                $failure = 'version_probe_format_invalid'
                if ($text -notmatch ('^' + [regex]::Escape($Prefix) + ' (\d+\.\d+\.\d+)$')) { throw $failure }
                if ($timer.Elapsed.TotalSeconds -ge 3) { $failure='version_probe_deadline'; throw $failure }
                $success = $true
                return $Matches[1]
            }
            Start-Sleep -Milliseconds 10
        }
        $failure='version_probe_deadline'
        throw $failure
    } catch {
        throw $failure
    } finally {
        if ($process) {
            if (-not $success) {
                $exited = $false
                try {
                    if (-not $process.HasExited) { $process.Kill() }
                    $exited = $process.WaitForExit(2000)
                } catch { }
                if (-not $exited) { $process.Dispose(); throw ($failure + '; owned_exit_unverified') }
            }
            $process.Dispose()
        }
    }
}
