# DevCore BaseOS installer defaults.
#
# This intentionally omits clearpart, autopart, user, rootpw, and package
# commands. Anaconda remains graphical and requires the installer user to
# review the target disk, partition layout, locale, network, and account
# choices. The installed payload starts DevCore's native first-boot wizard.
graphical
eula --agreed
lang en_US.UTF-8
keyboard us
timezone UTC --utc
network --bootproto=dhcp --device=link --activate --onboot=on
