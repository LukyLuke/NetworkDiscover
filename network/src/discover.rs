
use crate::hosts::Host;
use local_net::LocalNet;
use config::AppConfig;
use log::info;

const MAX_TRACEROUTE_HOPS: u16 = 15;

/// Scan for all hosts from the given target networks, scan them for services, vulnerabilities and windows information.
/// The result is saved into the database and is not returned.
///
/// # Arguments
///
/// * `db` - Database connection used to save all information with
/// * `local` - The local network configuration
/// * `config` - Reference to the main configuration
///
/// # Example
///
/// ```
/// start(&db, local, config);
/// ```
pub fn start(db: &mut sqlite::Database, local: &LocalNet, config: &AppConfig) {
	info!("HostDiscovery: start");
	scan_hosts(db, &config, discover_impl::find_hosts(local, &config.targets, config.script_args.clone().unwrap_or_default()), config.num_threads);
	info!("HostDiscovery: ended");
}

/// Scan and return a list of hosts, grouped in sublists, for services, vulnerabilities and windows information.
///
/// # Arguments
///
/// * `db` - Database connection used to save all information with
/// * `config` - Reference to the main configuration
/// * `hosts` - Hosts to scan in parallel
/// * `threads` - Number of Threads
///
/// # Example
///
/// ```
/// let grouped: Vec<Vec<Host>> = vec![];
/// let scanned = scanned_hosts(&db, &config, grouped, 5);
/// ```
pub fn scan_hosts(db: &mut sqlite::Database, config: &AppConfig, hosts: Vec<Host>, threads: u32) -> Vec<Host> {
	let scanned_hosts = discover_impl::scan_hosts(db, hosts, threads as usize);
	discover_impl::enummerate_windows(db, &config, scanned_hosts.clone());
	scanned_hosts
}

mod discover_impl {
	use crate::hosts::{Host, Service, Vulnerability, Protocol, State, Windows, WindowsInfo, WindowsDomain, WindowsShare, WindowsPrinter, ScriptElement};
	use local_net::LocalNet;
	use config::AppConfig;
	use log::{info, debug, trace};

	use std::process::Command;
	use std::net::IpAddr;
	use std::str::FromStr;
	use std::env::temp_dir;
	use std::path::PathBuf;
	use std::fs::File;
	use std::thread;
	use std::sync::{Arc, Mutex};
	use std::io::{BufReader, prelude::*};
	use std::collections::HashMap;
	use xml::reader::{EventReader, XmlEvent};
	use uuid::Uuid;
	use serde_json;

	/// Scans the Network for Hosts and returns a list a given number of grouped hosts.
	///
	/// # Arguments
	///
	/// * `local` - Local network configuration
	/// * `targets` - List of targets and/or Networks to find Hosts on and scan
	/// * `scan_arguments` - Arguments to pass to the scanner
	///
	/// # Example
	///
	/// ```
	/// let hosts = find_hosts_chunked(local, targest);
	/// let hosts = scan_hosts(&db, hosts, 5);
	/// ```
	pub(crate) fn find_hosts(local: &LocalNet, targets: &Vec<config::DiscoverStruct>, scan_arguments: String) -> Vec<Host> {
		let mut result: Vec<Host> = vec![];
		for target in targets {
			for host in discover_network(local, &target, scan_arguments.clone()) {
				result.push(host);
			}
		}
		result
	}

	/// Returns a List of all hosts after they where scanned for services and vulnerabilities.
	/// First routing information are gathered, after each host scanned for services and optionally for vulnerabilities.
	///
	/// # Arguments
	///
	/// * `db` - Database connection to use to save the hosts after all scans are done
	/// * `hosts` - List of host-lists; Each List is used in a thread to scan in parallel
	/// * `threads` - The number of threads to use for scanning
	///
	/// # Example
	///
	/// ```
	/// let hosts = find_hosts_chunked(lcoal, targest);
	/// let hosts = scan_hosts(&db, hosts, 5);
	/// ```
	pub(crate) fn scan_hosts(db: &mut sqlite::Database, hosts: Vec<Host>, threads: usize) -> Vec<Host> {
		let mut result: Vec<Host> = vec![];
		let num_threads = if threads > hosts.len() { hosts.len() } else { threads };
		let mut handles = vec![];
		let queue = Arc::new(Mutex::new(hosts));

		debug!("[Scan] Threads: {}", num_threads);
		for _ in 0..num_threads {
			let mutex = Arc::clone(&queue);
			let handle = thread::spawn(move || {
				debug!("[Scan] Thread {:?} started", thread::current().id());
				let mut res: Vec<Host> = vec![];
				loop {
					// Lock and free the mutex after taking the ownerhip of the last entry
					let host = {
						let mut hosts = mutex.lock().unwrap();
						hosts.pop()
					};
					// Process the host or end the thread
					match host {
						Some(mut host) => {
							debug!("[Scan] Thread {:?} Processing {:?}", thread::current().id(), host.ip);
							traceroute(&mut host);
							portscan(&mut host);
							res.push(host.clone());
						},
						None => break
					}
				}
				res
			});
			handles.push(handle);
		}

		// Join and wait till every thread is finished
		for handle in handles {
			debug!("[Scan] Thread joined: {:?}", &handle.thread().id());
			handle.join().unwrap()
				.iter_mut()
				.for_each(|host| {
					host.save_to_db(db);
					host.update_host_information(db);
					host.save_services_to_db(db);
					host.save_scripts_to_db(db);
					result.push(host.clone());
				});
		}
		result
	}

	/// Tries to enummerate windows information on all given hosts in parallel.
	///
	/// The function is blocking until all threads are finished up.
	/// The information is saved for all hosts in a thread after it finished.
	///
	/// # Arguments
	///
	/// * `db` - Database conenction to use to save the hosts after information where enummerated
	/// * `config` - Reference to the main configuration
	/// * `hosts` - List of hosts to try to enummerate windows information
	///
	/// # Example
	///
	/// ```
	/// enummerate_windows(&db, &config, hosts);
	/// ```
	pub(crate) fn enummerate_windows(db: &mut sqlite::Database, config: &AppConfig, hosts: Vec<Host>) {
		let threads = config.num_threads as usize;
		let num_threads = if threads > hosts.len() { hosts.len() } else { threads };
		let mut handles = vec![];
		let queue = Arc::new(Mutex::new(hosts));

		debug!("[Windows] Threads: {}", num_threads);
		for _ in 0..num_threads {
			let conf = config.clone();
			let mutex = Arc::clone(&queue);
			let handle = thread::spawn(move || {
				debug!("[Windows] Thread {:?} started", thread::current().id());
				let mut res: Vec<Host> = vec![];
				loop {
					// Lock and free the mutex after taking the ownership of the last entry
					let host = {
						let mut hosts = mutex.lock().unwrap();
						hosts.pop()
					};
					// Process the host or end the thread
					match host {
						Some(host) => { enumerate_windows_information(&conf, &host).map(|h| res.push(h.clone())); },
						None => break
					}
				}
				res
			});
			handles.push(handle);
		}

		// Join and wait till every thread is finished
		for handle in handles {
			debug!("[Windows] Thread joined: {:?}", &handle.thread().id());
			handle.join().unwrap()
				.iter()
				.for_each(|host| host.save_windows_information(db) );
		};
	}

	/// Scan a host for windows services, shares, printers, general information
	///
	/// Based on the configuration if the host, the user, password, workgroup is omitted.
	///
	/// This function creates system commands:
	///
	/// * `enum4linux IP.AD.DR.ES -A -C -d -oJ TMP_FILE -u USER -w WORKGROUP -p PASSWORD`
	///
	/// # Arguments
	///
	/// * `config` - Reference to the main configuration
	/// * `host` - The Host to scan for windows services
	///
	/// # Example
	///
	/// ```
	/// let windows_host = enumerate_windows_information(&config, &host);
	/// ```
	fn enumerate_windows_information(config: &AppConfig, host: &Host) -> Option<Host> {
		let target = config.targets.iter()
			.filter(|t| t.is_responsible_for(host.ip.unwrap_or(IpAddr::from_str("127.0.0.1").unwrap())))
			.next();

		let tmp_file = get_tmp_file(None);
		let mut cmd = Command::new("enum4linux-ng");
		cmd.arg(host.ip.unwrap().to_string())
			.arg("-A")
			.arg("-C")
			.arg("-d")
			.arg("-oJ")
			.arg(&tmp_file);
		if let Some(t) = &target {
			if let Some(win) = &t.windows {
				if let Some(val) = &win.domain_user { cmd.arg("-u").arg(val); }
				if let Some(val) = &win.password { cmd.arg("-p").arg(val); }
				if let Some(val) = &win.domain { cmd.arg("-w").arg(val); }
			}
		}
		trace!("[{:?}] Command: {:?}", thread::current().id(), cmd);

		let output = cmd.output();
		match output {
			Ok(_) => {
				let json_file = PathBuf::from( format!("{}.json", tmp_file.to_str().unwrap()) );
				let reader = BufReader::new(File::open(&json_file).unwrap());
				let parsed: serde_json::Value = serde_json::from_reader(reader).unwrap_or(serde_json::from_str("{}").unwrap());

				let info = match &parsed.get("os_info") {
					None => None,
					Some(info) => Some(WindowsInfo {
							native_lan_manager: info["Native LAN manager"].as_str().map(|s| String::from(s)).filter(|s| !s.is_empty()),
							native_os: info["Native OS"].as_str().map(|s| String::from(s)).filter(|s| !s.is_empty()),
							os_name: info["OS"].as_str().map(|s| String::from(s)).filter(|s| !s.is_empty()),
							os_build: info["OS build"].as_str().map(|s| String::from(s)).filter(|s| !s.is_empty()),
							os_release: info["OS release"].as_str().map(|s| String::from(s)).filter(|s| !s.is_empty()),
							os_version: info["OS version"].as_str().map(|s| String::from(s)).filter(|s| !s.is_empty()),
							platform: info["Platform id"].as_str().map(|s| String::from(s)).filter(|s| !s.is_empty()),
							server_type: info["Server type"].as_str().map(|s| String::from(s)).filter(|s| !s.is_empty()),
							server_string: info["Server type string"].as_str().map(|s| String::from(s)).filter(|s| !s.is_empty()),
						}),
				};
				let domain = match &parsed.get("smb_domain_info") {
					None => None,
					Some(info) => Some(WindowsDomain {
							domain: parsed["domain"].as_str().map(|s| String::from(s)).filter(|s| !s.is_empty()),
							dns_domain: info["DNS domain"].as_str().map(|s| String::from(s)).filter(|s| !s.is_empty()),
							derived_domain: info["Derived domain"].as_str().map(|s| String::from(s)).filter(|s| !s.is_empty()),
							derived_membership: info["Derived membership"].as_str().map(|s| String::from(s)).filter(|s| !s.is_empty()),
							fqdn: info["FQDN"].as_str().map(|s| String::from(s)).filter(|s| !s.is_empty()),
							netbios_name: info["NetBIOS computer name"].as_str().map(|s| String::from(s)).filter(|s| !s.is_empty()),
							netbios_domain: info["NetBIOS domain name"].as_str().map(|s| String::from(s)).filter(|s| !s.is_empty()),
						}),
				};
				let shares = match &parsed["shares"].as_object() {
					None => vec![],
					Some(shares) => {
						shares.iter().map(|(key, value)| WindowsShare {
							name: Some(String::from(key)),
							comment: value["comment"].as_str().map(|s| String::from(s)),
							share_type: value["type"].as_str().map(|s| String::from(s)),
							access: HashMap::new(), // TODO: Do we need this?
						}).collect::<Vec<WindowsShare>>()
					}
				};
				let printers = match &parsed["printers"].as_object() {
					None => vec![],
					Some(printers) => {
						printers.iter().map(|(key, value)| WindowsPrinter {
							uri: Some(String::from(key)),
							comment: value["comment"].as_str().map(|s| String::from(s)),
							description: value["description"].as_str().map(|s| String::from(s)),
							flags: value["flags"].as_str().map(|s| String::from(s)),
						}).collect::<Vec<WindowsPrinter>>()
					}
				};
				std::fs::remove_file(json_file).unwrap_or_default();

				match info.is_some() {
					true => {
						let mut windows_host = host.clone();
						windows_host.windows = Some(Windows { info, domain, shares, printers });
						Some(windows_host)
					},
					false => None
				}
			},
			Err(_) => None
		}
	}

	/// Returns a unique filename as a PathBuf in the systems temp folder.
	/// The File is not created, it's just the filename and path.
	///
	/// # Arguments
	///
	/// * `extension` - Optional File-Extension
	///
	/// # Example
	///
	/// ```
	/// let tmp_xml_file = get_tmp_file(Some("xml"));
	/// let tmp_file = get_tmp_file(None);
	/// ```
	fn get_tmp_file(extension: Option<&str>) -> PathBuf {
		let mut tmp_dir = temp_dir();
		let tmp_name = format!("{}{}{}", Uuid::new_v4(), if extension.is_some() { "." } else {""}, extension.unwrap_or_default());
		tmp_dir.push(tmp_name);
		tmp_dir
	}

	/// Returns the content of a given file as a String and removes the file afterwards.
	///
	/// # Arguments
	///
	/// * `path` - Location of the file to read and remove
	///
	/// # Example
	///
	/// ```
	/// let lines = get_file_content_and_cleanup(&tmp_file);
	/// ```
	fn get_file_content_and_cleanup(path: &PathBuf) -> String {
		let file = File::open(path);
		if file.is_ok() {
			let mut f = file.unwrap();
			let mut lines = String::new();
			f.read_to_string(&mut lines).unwrap();
			let _ = std::fs::remove_file(path);
			return lines;
		}
		String::new()
	}

	/// Returns all online hosts from a given network.
	///
	/// This function creates system commands:
	///
	/// * `sudo nmap -sn -oX /tmp/...xml HOST`
	///
	/// # Arguments
	///
	/// * `local` - The local network configuration used to get routing information
	/// * `target` - The target network to search all online hosts
	/// * `scan_arguments` - Arguments to apss to the scanner
	///
	/// # Example
	///
	/// ```
	/// for host in discover_network(local, &target) {
	///     info!("Found Host: {:?}", host);
	/// }
	/// ```
	fn discover_network(local: &LocalNet, target: &config::DiscoverStruct, scan_arguments: String) -> Vec<Host> {
		let mut result: Vec<Host> = vec![];
		let default_network = local.default_network()
			.unwrap()
			.default_route()
			.unwrap()
			.to_string();
		let network = &target.target.as_ref()
			.unwrap()
			.get_network_string()
			.unwrap_or(default_network);
		info!("Network: {}", network);

		let tmp_file = get_tmp_file(Some("xml"));
		let mut cmd = Command::new("sudo");
		cmd.arg("nmap")
			.arg("-sn")
			.arg("-oX")
			.arg(&tmp_file)
			.arg(&network);
		trace!("[{:?}] Command: {:?}", thread::current().id(), cmd);

		let output = cmd.output();
		if output.is_ok() {
			let lines = get_file_content_and_cleanup(&tmp_file);
			let parser = EventReader::from_str(&lines);

			for ev in parser {
				match ev {
					Ok(XmlEvent::StartElement { name, attributes, .. }) => {
						if name.local_name == "address" {
							let is_ip_tag = attributes.iter()
								.filter(|a| a.name.local_name == "addrtype" && (a.value == "ipv4" || a.value == "ipv6"))
								.map(|_| true)
								.take(1).next()
								.unwrap_or(false);
							if is_ip_tag {
								let mut host = Host::default();
								host.network = String::from(network);
								host.extended_scan = target.extended.unwrap_or(true);
								host.version_check = target.version_check.unwrap_or(true);
								host.scan_arguments = scan_arguments.clone();
								host.ip = Some(
									attributes.iter()
										.filter(|a| a.name.local_name == "addr")
										.map(|a| String::from(&a.value))
										.inspect(|ip| debug!("  found: {}", ip))
										.map(|ip| IpAddr::from_str(&ip))
										.take(1).next()
										.unwrap_or(IpAddr::from_str("127.0.0.1"))
										.unwrap()
								);
								result.push(host);
							}
						}
					},
					_ => {},
				}
			}
			info!("Found: {:?} hosts", result.len());
		}
		return result;
	}

	/// Traces and fills routing information on the given host object
	///
	/// This function creates system commands:
	///
	/// * `ip route list match IP.AD.DR.ES`
	/// * `traceroute -n -q1 -m IP.AD.DR.ES`
	///
	/// # Arguments
	///
	/// * `host` - Host to trace and find the route to
	///
	/// # Example
	///
	/// ```
	/// traceroute(&mut host);
	/// ```
	/// TODO: Is it more Rust-Style if the host is returned and not modified?
	fn traceroute(host: &mut Host) {
		if host.ip.is_some() && !host.hops.iter().any(|hop| host.ip.unwrap().to_string() == hop.to_string()) {
			let ip = host.ip.unwrap().to_string();
			debug!("Host: {:?}", ip);

			// Default-Gateway to reach the host
			let mut cmd = Command::new("ip");
			cmd.arg("route")
				.arg("list")
				.arg("match")
				.arg(ip.clone());
			trace!("[{:?}] Command: {:?}", thread::current().id(), cmd);

			if let Ok(output) = cmd.output() {
				let mut gateways: Vec<(String, String)> = vec![];
				let mut device: Option<(String, String)> = None;
				let lines = String::from_utf8(output.stdout).unwrap();
				for line in lines.lines() {
					let parts = line.trim().split_whitespace().collect::<Vec<&str>>();
					if parts.get(0).unwrap_or(&"") == &"default" {
						gateways.push( ( String::from(parts.get(2).unwrap_or(&"").to_string()), String::from(parts.get(4).unwrap_or(&"").to_string()) ) );
					} else {
						device = Some( ( String::from(parts.get(2).unwrap_or(&"").to_string()), String::from(parts.get(0).unwrap_or(&"").to_string())  ) );
					}
				}

				// Direct Route -> Get the Gateway and Network based on the the "device"
				let mut gateway = if let Some((ref device, ref network)) = device {
					gateways.clone().into_iter().find_map(|(gw, dev)| if &dev == device {
						host.ipnet = network.clone();
						Some(gw)
					} else {
						None
					} )
				} else { None };

				// If no direct route was found, take the first default gateway
				if gateway.is_none() {
					gateway = Some(gateways.into_iter().next().unwrap_or_default().0)
				}

				// TODO: If no network could be evaluated from the routing table (only possible on direct connected networks), try to calculate

				debug!("  gateway: {:?}", gateway.clone().unwrap_or(String::from("Unknown")));
				if let Some(gateway) = gateway {
					host.hops.push(IpAddr::from_str(gateway.as_str()).unwrap());
				}
			}

			// Traceroute the host
			let mut cmd = Command::new("traceroute");
			cmd.arg("-n")  // no name lookup
				.arg("-q 1") // only one query
				.arg("-m").arg(super::MAX_TRACEROUTE_HOPS.to_string()) // max hops
				.arg(ip.clone());
			trace!("[{:?}] Command: {:?}", thread::current().id(), cmd);

			let output = cmd.output();
			if output.is_ok() {
				let lines = String::from_utf8(output.unwrap().stdout).unwrap();
				for line in lines.lines() {
					let mut parts = line.trim().split_whitespace().collect::<Vec<&str>>();
					// First part has to be a number and the one should not start with a exclamation mark like "!H"
					if parts.get(0).unwrap_or(&"none").parse::<u16>().is_ok() && parts.pop().unwrap_or(&"a").chars().next().unwrap() != '!' {
						// In case of a " * " line, the * was removed above
						match IpAddr::from_str(parts.get(1).unwrap_or(&"*")) {
							Ok(ip) => host.hops.push(ip),
							_ => {},
						}
					}
				}
			}
			
			// In case the host is not traceable, add it as the last hop
			if !host.hops.contains(&host.ip.unwrap()) {
				host.hops.push(host.ip.unwrap());
			}
			debug!("  route: {:?}", host.hops);
		} else {
			debug!("  no route to: {:?}", host.ip);
		}
	}

	/// Scans a host for open ports and services.
	///
	/// If the host is flagged as `extended_scan`, also a vulnerability scan is performed with the `vulners.nse` nmap script
	///
	/// This function creates system commands:
	///
	/// * `nmap -O -sT -sV --script=vulners.nse`
	///
	/// # Arguments
	///
	/// * `host` - Host to scan for services and vulnerabilities
	///
	/// # Example
	///
	/// ```
	/// portscan(&mut host);
	/// ```
	/// TODO: Is it more Rust-Style if the host is returned and not modified?
	fn portscan(host: &mut Host) {
		if host.ip.is_some() {
			let ip = host.ip.unwrap().to_string();
			debug!("Portscan: {:?}", ip);
			
			let tmp_file = get_tmp_file(Some("xml"));
			let mut cmd = Command::new("sudo");
			cmd.arg("nmap")
				.arg("-O")
				.arg("-sT");
			if host.version_check || host.extended_scan {
				cmd.arg("-sV");
			}
			if !host.scan_arguments.is_empty() {
				let mut sargs = String::from("--script-args=");
				sargs.push_str(host.scan_arguments.as_str());
				cmd.arg(sargs.as_str());
			}
			if host.extended_scan {
				cmd.arg("--script=vulners");
			} else {
				cmd.arg("--script=./scripts/");
			}
			cmd.arg("-oX")
				.arg(&tmp_file)
				.arg(ip);
			trace!("[{:?}] Command: {:?}", thread::current().id(), cmd);

			let output = cmd.output();
			if output.is_ok() {
				let mut service = Service::default();
				let mut vulners = Vulnerability::default();

				let mut is_script = false;
				let mut script_id = String::from("");
				let mut script_elem_key = String::from("");

				let lines = get_file_content_and_cleanup(&tmp_file);
				let parser = EventReader::from_str(&lines);
				for ev in parser {
					match ev {
						Ok(XmlEvent::StartElement { name, attributes, .. }) => {
							if name.local_name == "port" {
								service.port = attributes.iter()
									.filter(|a| a.name.local_name == "portid")
									.map(|a| a.value.parse::<u16>().unwrap_or_default())
									.take(1).next()
									.unwrap_or_default();
								service.protocol = attributes.iter()
									.filter(|a| a.name.local_name == "protocol")
									.map(|a| {
										if a.value == "tcp" { return Protocol::TCP; }
										else if a.value == "udp" { return Protocol::UDP; }
										Protocol::UNKNOWN
									}).take(1).next().unwrap_or(Protocol::UNKNOWN);

							} else if name.local_name == "state" {
								service.state = attributes.iter()
									.filter(|a| a.name.local_name == "state")
									.map(|a| {
										if a.value == "open" { return State::OPEN; }
										else if a.value == "filter" { return State::FILTER; }
										else if a.value == "close" { return State::CLOSE; }
										State::UNKNOWN
									}).take(1).next().unwrap_or(State::UNKNOWN);

							} else if name.local_name == "service" {
								service.name = attributes.iter()
									.filter(|a| a.name.local_name == "name")
									.map(|a| String::from(&a.value))
									.take(1).next()
									.unwrap_or_default();

								service.product = attributes.iter()
									.filter(|a| a.name.local_name == "product")
									.map(|a| String::from(&a.value))
									.take(1).next()
									.unwrap_or_default();

								service.version = attributes.iter()
									.filter(|a| a.name.local_name == "version")
									.map(|a| String::from(&a.value))
									.take(1).next()
									.unwrap_or_default();

							} else if name.local_name == "osmatch" {
								host.os = attributes.iter()
									.filter(|a| a.name.local_name == "name")
									.map(|a| String::from(&a.value))
									.take(1).next();

							} else if name.local_name == "script" {
									is_script = true;
									script_id =  attributes.iter()
										.filter(|a| a.name.local_name == "id")
										.map(|a| String::from(&a.value))
										.take(1).next()
										.unwrap_or_default();

							} else if name.local_name == "elem" {
								script_elem_key = attributes.iter()
									.map(|a| String::from(&a.value))
									.take(1).next()
									.unwrap_or_default();
							}
						},

						Ok(XmlEvent::Characters(value)) if is_script => {
							match script_id.as_str() {
								"vulners" => match script_elem_key.as_str() {
									"id" => vulners.id = value,
									"type" => vulners.database = value,
									"is_exploit" => vulners.exploit = value.parse::<bool>().unwrap_or(false),
									"cvss" => vulners.cvss = value.parse::<f32>().unwrap_or_default(),
									_ => {}
								},
								_ if !script_elem_key.is_empty() => {
									if !host.scripts.contains_key(&script_id) {
										host.scripts.insert(script_id.clone(), vec![]);
									}
									if let Some(entry) = host.scripts.get_mut(&script_id) {
										entry.push(ScriptElement {
											key: script_elem_key.clone(),
											value: value.clone(),
										});
									}
								},
								_ => {}
							};
						},

						Ok(XmlEvent::EndElement { name }) => {
							if name.local_name == "port" {
								debug!("  service: {:?}", &service);
								host.services.push(service);
								service = Service::default();

							} else if is_script && name.local_name == "table" && !vulners.id.is_empty() {
								service.vulns.push(vulners.clone());
								vulners = Vulnerability::default();

							} else if is_script && name.local_name == "script" {
								is_script = false;
							}
						},
						_ => {}
					}
				}

			}
		}
	}
	
}
