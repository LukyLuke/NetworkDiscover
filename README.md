# Network Discover

Network Discover is a lightweight server application which uses `ip`, `nmap` with `vulners.nse` and `traceroute` to discover all hosts in a given network and checks for vulnerabilities.

To be abe to run `nmap` with more privileges, `sudo` is used. Therefore the following file ahst to be created and the User which starts network Discovery must be in the _sudo_ group. Any other configuration is possible too.

_/etc/sudoers.d/nmap_
```
Cmnd_Alias NMAP = /usr/bin/nmap
%sudo ALL=(ALL) NOPASSWD: NMAP
```


## Features

* Discovers all running Hosts on one/multiple networks
* Creates a Network map based on traceroute
* Checks every Host for the running Operating System
* Checks every Host for open and running services
* Checks every running service for known Vulnerabilities (via vulners)
* Scans can run manually or automatically every given timespan
* CSV Export of a single scan

### Planned Features

* Detect new Hosts based on DHCP and SLAAC messages
* Shows a diff between two (or more?) scans
* Scan Windows Systems for NetBios and other vulnerabilities (enum4linux?)
* SNMP information gathering and display
* Better Network Map visualization ( https://d3js.org/ / https://observablehq.com/@d3/force-directed-tree )
* PDF Reporting
* Scan a host individually (predefined nmap params or individual?)
* ...

### Maybe Features

* Smoke-Ping similar connectivity test
* Split it up into Server part and a library to be usable as WASM maybe
* ...


## Configuration

The Configuration is created automaticall on first start. I is stored under `/.config/network_discover/network_discover.toml`

```toml
name = 'LocalNet'
repeat = 0
num_threads = 10

[listen]
ip = '0.0.0.0'
mask = 32
port = 9090
protocol = 'UDP'

[[targets]]
extended = false
max_hops = 3

[targets.target]
ip = '192.168.66.0'
name = 'Local LAN'
mask = 24
port = 53
protocol = 'UDP'

[targets.target]
ip = '192.168.55.0'
name = 'Guest WLAN'
mask = 24
port = 53
protocol = 'UDP'
```

### Main Parameters

* **name** _(string)_: Name of the scanner instance
* **repeat** _(int32)_: Number of seconds to pause after a new scan starts
* **num_threads** _(int32)_: Number of Threads used to run nmap scans against found hosts

### Listen Parameters

* **ip** _(string)_: IP-Address to listen on for the Web-Interface
* **port** _(int32)_: Port on which the Webserver should listen on
* **mask** _(int32)_: _(not used)_
* **protocol** _(string)_: _(not used)_

### Targets

* **extended** _(bool)_: If false, only a simple scan is run on the targets, otherwise a full scan
* **max_hops** _(int32)_: Maximum numbers of hops to reach a target host

#### List of Targets

* **ip** _(string)_: The network address to scan
* **name** _(string)_: Name of the network
* **mask** _(int32)_: Netmask as CIDR for the above network address
* **port** _(int32)_: Port to use for check if a host is alive
* **protocol** _(string)_: Protocol (TCP, UDP) to use for check if a host is alive