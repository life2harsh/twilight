## Download

| You want to | Windows | Linux |
|---|---|---|
| Watch and control another machine | `Twilight-windows.exe` | `Twilight-linux` |
| Share this machine's screen | `twilight-host-windows.exe` | `twilight-host-linux` |

Nothing to install. Both machines must be on the same network.

1. On the machine to share, run the host. It prints a PIN. Find its IP with `ipconfig` on
   Windows or `ip a` on Linux.
2. On the other machine, run Twilight, enter that IP and PIN, then press Connect.
3. Close the video window to disconnect.

The first time it runs, Windows may say "Windows protected your PC". Click **More info**, then
**Run anyway**. The exes are not signed. When the firewall asks, allow the host on private
networks.

On Linux, run `chmod +x` on the files first.
