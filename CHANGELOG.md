# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
* (Breaking) Endpoint 0 is no longer fully owned by the stack. The user handler now provides the
  system clusters that do not depend on the operational network (Descriptor, Basic Information,
  Administrator Commissioning, Operational Credentials, Access Control, Group Key Management,
  Software Diagnostics, Time Synchronization) - `MatterStack::root_handler()` returns a chain with
  all of them - while the stack chains only Network Commissioning, General Commissioning,
  General Diagnostics and the network-type diagnostics cluster on top. This makes it possible
  to add custom clusters to Endpoint 0 (Diagnostic Logs, ICD Management, OTA, ...).
* Update to the latest rs-matter: handler chain matchers are closures now
  (`|e, c| e == LIGHT_ENDPOINT_ID && c == OnOffHandler::CLUSTER.id`)
* (Breaking) Advertise **all** IPv6 addresses of the operational network interface over mDNS,
  as the latest rs-matter does: `Mdns::run` takes `ipv6: &[Ipv6Addr]`

## [0.2.0] - 2026-08-20
* Advertise over BLE only when the comm window is open
* Update to latest rs-matter v0.3
* Default the events ringbuffers to 256 bytes each

## [0.1.0] - 2026-06-25
* Initial release
