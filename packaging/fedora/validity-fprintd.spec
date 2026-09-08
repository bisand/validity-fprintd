%global _description %{expand:
A Rust driver and fprintd-compatible D-Bus daemon for Synaptics/Validity
match-on-chip fingerprint sensors, the family that stock libfprint does not
support. It serves the same D-Bus interface as fprintd, so pam_fprintd and the
standard fprintd clients work against it unchanged.}

Name:           validity-fprintd
Version:        0.1.0
Release:        %autorelease
Summary:        Fingerprint driver for Synaptics/Validity match-on-chip sensors

License:        MIT
URL:            https://github.com/bisand/validity-fprintd
Source0:        %{url}/archive/refs/tags/v%{version}.tar.gz#/%{name}-%{version}.tar.gz

ExclusiveArch:  x86_64 aarch64

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  libusb1-devel
BuildRequires:  systemd-rpm-macros

Requires:       dbus
# fprintd provides pam_fprintd and the fprintd-* clients. This daemon serves
# the same D-Bus interface, so it conflicts with fprintd's own service rather
# than with the package; see the note printed after installation.
Recommends:     fprintd-pam

%description %{_description}

%prep
%autosetup -n %{name}-%{version}

%build
# Deliberately not --features vendored: link the system libusb so its security
# updates apply here too.
%{cargo_env}
cargo build --release --locked

%install
for b in validity-fprintd validity-probe validity-session validity-db \
         validity-sensor validity-verify validity-baseline \
         validity-firmware validity-provision; do
    install -Dpm0755 target/release/$b %{buildroot}%{_bindir}/$b
done

# The shipped unit points at /usr/local/bin, where the install script puts a
# source build; a package installs to %{_bindir}.
sed 's|/usr/local/bin/|%{_bindir}/|' packaging/validity-fprintd.service \
    > %{name}.service
install -Dpm0644 %{name}.service %{buildroot}%{_unitdir}/%{name}.service

install -Dpm0644 udev/70-validity-fprintd.rules \
    %{buildroot}%{_udevrulesdir}/70-validity-fprintd.rules
install -Dpm0755 scripts/fetch-firmware.sh \
    %{buildroot}%{_datadir}/%{name}/fetch-firmware.sh

%post
%systemd_post %{name}.service
if [ $1 -eq 1 ]; then
cat <<'MSG'

validity-fprintd is installed but not yet enabled.

It claims the same D-Bus name as fprintd and the two cannot run together.
Masking fprintd also stops D-Bus activating it:

  systemctl mask --now fprintd.service
  systemctl enable --now validity-fprintd.service

Check that your sensor is recognised:

  systemctl stop validity-fprintd
  validity-provision
  systemctl start validity-fprintd

For fingerprint login, reference pam_fprintd from /etc/pam.d, keeping it
"sufficient" so a failed finger falls through to the password. On Fedora,
authselect enable-feature with-fingerprint does this for you.

MSG
fi

%preun
%systemd_preun %{name}.service

%postun
%systemd_postun_with_restart %{name}.service
if [ $1 -eq 0 ]; then
    # Only on final removal, not on upgrade.
    systemctl unmask fprintd.service >/dev/null 2>&1 || :
fi

%files
%license LICENSE
%doc README.md
%{_bindir}/validity-*
%{_unitdir}/%{name}.service
%{_udevrulesdir}/70-validity-fprintd.rules
%dir %{_datadir}/%{name}
%{_datadir}/%{name}/fetch-firmware.sh

%changelog
%autochangelog
