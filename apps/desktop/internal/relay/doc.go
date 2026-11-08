// Package relay implements the self-hostable ukk control-plane relay.
//
// The relay is an untrusted router. It authenticates the operator's host
// connection, routes opaque encrypted frames between one host and its devices,
// and never decodes, inspects or logs frame payloads. It has no persistence and
// no authority over grants or pairing.
//
// Endpoints:
//
//	GET /healthz   liveness
//	GET /host      WebSocket; Authorization: Bearer <operator token>
//	GET /device    WebSocket
//
// After the WebSocket upgrade the first frame must be hello:
//
//	{"t":"hello","host_id":"…"}                    host
//	{"t":"hello","host_id":"…","device_id":"…"}    device
//
// Registration happens once per connection. Control frames are:
//
//	{"t":"open","conn":"…"}                 device -> relay -> host
//	{"t":"data","conn":"…","payload":"…"}   either direction, base64 opaque
//	{"t":"close","conn":"…"}                either direction
//	{"t":"revoke","device":"…"}             host -> relay
//	{"t":"repair","device":"…"}             host -> relay (re-pair signal)
//	{"t":"presence","device_id":"…","online":true}  relay -> host
//	{"t":"presence","host_id":"…","online":true}    relay -> device
//	{"t":"error","code":"…","message":"…"}  relay -> peer
//
// revoke closes every connection for that device and refuses future
// registrations until the host sends repair. Errors carry stable codes and
// never contain payload bytes.
//
// Bounds: 16 MiB WebSocket message, 1 MiB payload, 1024 queued frames per
// connection, 8 connections per device, 32 devices per host, server pings and
// per-write deadlines.
package relay
