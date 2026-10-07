!macro customInstall
  DetailPrint "Configuring Windows Firewall rules for OpenRemote..."
  ExecWait 'netsh advfirewall firewall add rule name="OpenRemote Stream TCP" dir=in action=allow protocol=TCP localport=44321 profile=any'
  ExecWait 'netsh advfirewall firewall add rule name="OpenRemote Stream UDP" dir=in action=allow protocol=UDP localport=44321 profile=any'
  ExecWait 'netsh advfirewall firewall add rule name="OpenRemote Video Stream" dir=in action=allow protocol=TCP localport=44322 profile=any'
  ExecWait 'netsh advfirewall firewall add rule name="OpenRemote LAN Discovery" dir=in action=allow protocol=UDP localport=44320 profile=any'
  ExecWait 'netsh advfirewall firewall add rule name="OpenRemote Application" dir=in action=allow program="$INSTDIR\OpenRemote.exe" enable=yes profile=any'
!macroend

!macro customUnInstall
  DetailPrint "Removing Windows Firewall rules for OpenRemote..."
  ExecWait 'netsh advfirewall firewall delete rule name="OpenRemote Stream TCP"'
  ExecWait 'netsh advfirewall firewall delete rule name="OpenRemote Stream UDP"'
  ExecWait 'netsh advfirewall firewall delete rule name="OpenRemote Video Stream"'
  ExecWait 'netsh advfirewall firewall delete rule name="OpenRemote LAN Discovery"'
  ExecWait 'netsh advfirewall firewall delete rule name="OpenRemote Application"'
!macroend
