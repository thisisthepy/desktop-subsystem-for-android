# host/platform/windows

The Windows host: the Android VM, its control channel over Hyper-V sockets, and
`dsa-hostd`, which runs both.

## dsa-hostd

```
cargo build -p dsa-host-windows --bin dsa-hostd --release
dsa-hostd dsa-hostd.example.json
```

It creates the Android VM through the Host Compute System, waits for the guest's channel
daemon to connect on vsock port `control_port` and pass the protocol handshake, and prints
every state change. Type `start`, `stop`, `status` or `quit`; end of input quits, and
quitting stops the VM first.

- It needs administrator rights, because only administrators may create VMs through
  HCS. Run it from an elevated prompt. In the installed product the DSA service does this.
- It needs the Windows feature "Virtual Machine Platform". Without it, starting reports
  that the Host Compute System is unavailable and names the feature.
- `kernel` is the guest kernel from `dsa/kernel/` ("Kernel test" uploads it), and
  `system_image` and `data_disk` are the Android images, which are not built yet.

The settings are described in `src/host_config.rs`; unknown keys are an error. When
`user_sid` is absent, the user running `dsa-hostd` may use the VM's Hyper-V sockets.

### Serving users

```
dsa-hostd serve dsa-hostd.example.json
dsa-hostd service C:\ProgramData\DSA\dsa-hostd.json
```

`serve` answers the local API on the named pipe `\\.\pipe\<pipe_name>` (default
`dsa-host`) in the foreground and stops on Ctrl+C. `service` does the same under the
Service Control Manager; the installer registers it, and it writes its messages to
`log_file`.

- Each caller is identified by the SID Windows reports for the pipe connection, never by
  anything the caller sends. The first request of a user creates their data disk at
  `<data_dir>\<SID>\data.vhdx` and their VM `<vm_name>-<SID>`; later requests reach the
  same VM.
- A user's folder is readable only by SYSTEM, Administrators and Hyper-V's virtual
  machines, so other users cannot read someone's Android data.
- The pipe admits SYSTEM and interactively signed-in users unless `pipe_sddl` says
  otherwise. Remote clients are always refused.
- Stopping the service stops every user's VM first.

