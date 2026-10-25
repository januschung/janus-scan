# Janus Scan Server in an LXC container

The server runs directly in a Debian 13 container under systemd: no Docker inside. These steps use Proxmox VE; other LXC hosts (Incus, LXD) work the same way inside the container.

The host must be x86-64: Brother's driver doesn't run on ARM.

## 1. Build the files

On any machine with Docker, from the repo root:

```sh
docker build --platform linux/amd64 -f deploy/Dockerfile --target dist \
  --output type=local,dest=deploy/dist .
```

`deploy/dist/` then holds the server binary, the web page, `install.sh` and the systemd unit. Put Brother's `brscan4` amd64 `.deb` next to it (download it from support.brother.com; you accept Brother's license there).

## 2. Create the container (Proxmox)

Use the Debian 13 template (`pveam available | grep debian-13` lists its exact name), unprivileged, with its own address on the LAN:

```sh
pct create 120 local:vztmpl/<debian-13 template> --hostname janus-scan \
  --unprivileged 1 --cores 1 --memory 2048 --rootfs local-lvm:4 \
  --net0 name=eth0,bridge=vmbr0,ip=dhcp --start 1
```

2 GB of memory covers saving long documents: pages are held in memory while a file is written, about 25 MB each in color at 300 dpi. Reserve the container's address in the router.

## 3. NAS folders

Mount the share on the Proxmox host (`/etc/fstab`, or Datacenter → Storage), then pass it into the container:

```sh
pct set 120 -mp0 /mnt/nas/scans,mp=/nas/scans
```

The server runs as `janus-scan` (UID 1000 inside the container). In an unprivileged container that is UID **101000** on the host, so the share must let 101000 write: for SMB, mount it with `uid=101000,gid=101000`; for NFS, map writes on the NAS (for example `all_squash` with `anonuid`/`anongid`) or give 101000 write access.

## 4. Install

```sh
tar -C deploy -cf janus-scan.tar dist brscan4-0.4.11-1.amd64.deb
pct push 120 janus-scan.tar /root/janus-scan.tar
pct exec 120 -- sh -c 'cd /root && tar xf janus-scan.tar && sh dist/install.sh brscan4-0.4.11-1.amd64.deb'
```

Then inside the container (`pct enter 120`):

1. Set `SCANNER_IP` in `/etc/default/janus-scan`.
2. List the folders to save to in `/etc/janus-scan/server.yaml`, with the paths from step 3 (`/nas/scans`).
3. Set the time zone, so `{date}` and `{time}` in file names are local: `dpkg-reconfigure tzdata`.
4. `systemctl enable --now janus-scan`
5. Open `http://<container's IP>:8080` on a phone or computer.

`journalctl -u janus-scan` shows the log.

## Updating

Build `deploy/dist/` again, push and run `install.sh` the same way (it keeps `/etc/default/janus-scan` and `/etc/janus-scan/server.yaml`), then `systemctl restart janus-scan`.

## Notes

- **Scanned pages** in progress live in the service's private `/tmp`. They're deleted on New, after an hour without activity (`IDLE_TIMEOUT_MINUTES`), or when the service stops; saving keeps them until then.
