# Windows a3s-box nginx test commands

Use these commands in a normal Windows PowerShell window.

Before running these commands, make sure the Windows guest kernel file exists at
`D:\code\libkrun\src\libkrunfw-win\kernel\vmlinux`.

Setup guide:

- `D:\code\libkrun\src\libkrunfw-win\VMLINUX_SETUP.md`

## 1. Set runtime DLL path

```powershell
$env:PATH="D:\code\libkrun\target\x86_64-pc-windows-msvc\debug;$env:PATH"
```

## 2. Start nginx

```powershell
& "D:\code\a3s\crates\box\src\target\x86_64-pc-windows-msvc\debug\a3s-box.exe" run -d --name my-nginx -p 18080:80 docker.io/library/nginx:latest 1>$null 2>$null
```

## 3. Check port mapping

```powershell
& "D:\code\a3s\crates\box\src\target\x86_64-pc-windows-msvc\debug\a3s-box.exe" port my-nginx
```

## 4. Check nginx HTTP

```powershell
Invoke-WebRequest http://127.0.0.1:18080/
```

## 5. Check all boxes

```powershell
& "D:\code\a3s\crates\box\src\target\x86_64-pc-windows-msvc\debug\a3s-box.exe" ps -a
```

## 6. View nginx logs

```powershell
& "D:\code\a3s\crates\box\src\target\x86_64-pc-windows-msvc\debug\a3s-box.exe" logs --tail 100 my-nginx
```

## 7. Remove the test box

```powershell
& "D:\code\a3s\crates\box\src\target\x86_64-pc-windows-msvc\debug\a3s-box.exe" rm -f my-nginx
```

## 8. One-shot flow

```powershell
$env:PATH="D:\code\libkrun\target\x86_64-pc-windows-msvc\debug;$env:PATH"
& "D:\code\a3s\crates\box\src\target\x86_64-pc-windows-msvc\debug\a3s-box.exe" run -d --name my-nginx -p 18080:80 docker.io/library/nginx:latest 1>$null 2>$null
& "D:\code\a3s\crates\box\src\target\x86_64-pc-windows-msvc\debug\a3s-box.exe" port my-nginx
Invoke-WebRequest http://127.0.0.1:18080/
```
