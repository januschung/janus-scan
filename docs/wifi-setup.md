# Wi-Fi setup for the ADS-1000W

Brother's own Wi-Fi setup tool runs on Windows. You don't need it: the scanner has a built-in web page for its network settings, which works from any browser on macOS or Linux. This guide covers that page, plus how to get the scanner onto a network the first time.

## Which case are you in?

| Situation | What to do |
| --- | --- |
| The scanner is on your Wi-Fi and you want to change something (new router, new password) | [Change settings from the scanner's web page](#change-settings-from-the-scanners-web-page) |
| The scanner isn't on any network yet, and your router has a WPS button | [Use WPS](#use-wps) |
| The scanner isn't on any network yet, and there's no WPS | [Use ad-hoc mode](#use-ad-hoc-mode-not-yet-verified) |

## Find the scanner's IP address

Any of these work while the scanner is on your network:

- **Your router's admin page**, in its list of connected devices. The scanner's network name starts with `BRW`.
- **macOS:** run `dns-sd -B _scanner._tcp`, then `dns-sd -L "Brother ADS-1000W" _scanner._tcp` to get its address. Press Ctrl+C to stop each command.
- **Linux:** run `avahi-browse -rt _scanner._tcp`.

**Tip:** in your router's settings, reserve a fixed IP address for the scanner. On Linux, Brother's driver remembers the scanner by IP address, so a changed address makes it disappear until you register it again.

## Change settings from the scanner's web page

1. Open `http://<scanner IP>/` in a browser.
2. Go to **Network → Wireless (Personal)**. The page is `http://<scanner IP>/net/wireless/wireless.html`.
3. Set:
   - **Communication Mode:** Infrastructure
   - **Wireless Network Name (SSID):** your network's name
   - **Authentication Method:** WPA/WPA2-PSK
   - **Encryption Mode:** AES
   - the **password** for your network
4. Submit. The scanner leaves the current network and joins the new one.

If you mistype the new network's name or password, the scanner drops off the network. Get it back with [WPS](#use-wps) or [ad-hoc mode](#use-ad-hoc-mode-not-yet-verified).

The same page shows the scanner's current network, channel and signal strength, which helps when scans over Wi-Fi are slow or keep failing.

### Set an admin password

Out of the box, anyone on your network can open this page and change the scanner's settings. Set a password under **Administrator → Password** (`http://<scanner IP>/admin/password.html`).

## Use WPS

WPS lets the scanner join your router without typing the password.

1. Press the WPS button on your router (or turn on WPS in its admin page).
2. Within about two minutes, start WPS on the scanner. Brother's user guide for the ADS-1000W, on [support.brother.com](https://support.brother.com), describes the exact button steps for this model.
3. When the scanner has joined, find its IP address as described [above](#find-the-scanners-ip-address).

## Use ad-hoc mode (not yet verified)

The scanner supports **Ad-hoc** mode, where a computer connects to the scanner directly without a router. Many Brother devices without a screen start in ad-hoc mode after a network reset, and you can then open the same web page and switch them to your Wi-Fi. **This hasn't been tested with the ADS-1000W yet.**

Current macOS versions make joining ad-hoc networks awkward, so use a Linux computer if you try this.

## After the scanner is on Wi-Fi

- **macOS:** with Brother's scanner driver installed, Janus Scan finds the scanner automatically. It appears in the scanner list marked "Wi-Fi".
- **Linux:** register the scanner with Brother's driver once:

  ```sh
  sudo brsaneconfig4 -a name=ADS1000W model=ADS-1000W ip=<scanner IP>
  ```

  Check it with `scanimage -L`, which should list a `brother4:net1;dev0` device. Janus Scan then shows the scanner marked "Wi-Fi".

## Speed

Over Wi-Fi, Brother's macOS driver is much slower than USB: about 10 s per page side in color at 300 dpi, against 2 s over USB. On Linux, Brother's driver over the same Wi-Fi took about 3 s per page side. On a Mac, use USB for long documents.
