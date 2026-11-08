package relay

import (
	"sync"
)

type deviceKey struct {
	host   string
	device string
}

type connKey struct {
	host   string
	device string
	conn   string
}

type connState struct {
	key    connKey
	device *peer
	opened bool
}

// hub owns routing state. It never stores payload bytes.
type hub struct {
	srv *Server

	mu      sync.Mutex
	hosts   map[string]*peer
	devices map[deviceKey]map[*peer]struct{}
	revoked map[deviceKey]bool
	conns   map[connKey]*connState
}

func newHub(s *Server) *hub {
	return &hub{
		srv:     s,
		hosts:   make(map[string]*peer),
		devices: make(map[deviceKey]map[*peer]struct{}),
		revoked: make(map[deviceKey]bool),
		conns:   make(map[connKey]*connState),
	}
}

func (h *hub) addHost(p *peer, hostID string) *Error {
	h.mu.Lock()
	if h.srv.closed.Load() {
		h.mu.Unlock()
		return newError(CodeClosed, "relay closed")
	}
	if h.hosts[hostID] != nil {
		h.mu.Unlock()
		return newError(CodeHostAlreadyConnected, "host already connected")
	}
	p.hostID = hostID
	h.hosts[hostID] = p
	var devices []string
	for key := range h.devices {
		if key.host == hostID {
			devices = append(devices, key.device)
		}
	}
	var devicePeers []*peer
	for key, peers := range h.devices {
		if key.host != hostID {
			continue
		}
		for devicePeer := range peers {
			devicePeers = append(devicePeers, devicePeer)
		}
	}
	h.mu.Unlock()

	for _, deviceID := range devices {
		h.srv.sendPresenceToHost(p, deviceID, true)
	}
	for _, devicePeer := range devicePeers {
		h.srv.sendPresenceToDevice(devicePeer, hostID, true)
	}
	h.srv.emit(Event{Kind: EventHostConnected, HostID: hostID})
	return nil
}

func (h *hub) removeHost(p *peer) {
	h.mu.Lock()
	if h.hosts[p.hostID] != p {
		h.mu.Unlock()
		return
	}
	delete(h.hosts, p.hostID)
	var conns []*connState
	for key, state := range h.conns {
		if key.host == p.hostID {
			conns = append(conns, state)
			delete(h.conns, key)
		}
	}
	var devicePeers []*peer
	for key, peers := range h.devices {
		if key.host != p.hostID {
			continue
		}
		for devicePeer := range peers {
			devicePeers = append(devicePeers, devicePeer)
		}
	}
	h.mu.Unlock()

	for _, state := range conns {
		h.srv.enqueueFrame(state.device, frame{T: "close", Conn: state.key.conn})
		h.srv.emit(Event{Kind: EventConnClosed, HostID: state.key.host, DeviceID: state.key.device, ConnID: state.key.conn})
	}
	for _, devicePeer := range devicePeers {
		h.srv.sendPresenceToDevice(devicePeer, p.hostID, false)
	}
	h.srv.emit(Event{Kind: EventHostDisconnected, HostID: p.hostID})
}

func (h *hub) addDevice(p *peer, hostID, deviceID string) *Error {
	key := deviceKey{host: hostID, device: deviceID}
	h.mu.Lock()
	if h.srv.closed.Load() {
		h.mu.Unlock()
		return newError(CodeClosed, "relay closed")
	}
	if h.revoked[key] {
		h.mu.Unlock()
		return newError(CodeDeviceRevoked, "device revoked")
	}
	host := h.hosts[hostID]
	if host == nil {
		h.mu.Unlock()
		return newError(CodeHostOffline, "host not connected")
	}
	peers := h.devices[key]
	if peers == nil {
		count := 0
		for existing := range h.devices {
			if existing.host == hostID {
				count++
			}
		}
		if count >= MaxDevicesPerHost {
			h.mu.Unlock()
			return newError(CodeHostDeviceLimit, "host device limit reached")
		}
		peers = make(map[*peer]struct{})
		h.devices[key] = peers
	}
	if len(peers) >= MaxConnsPerDevice {
		h.mu.Unlock()
		return newError(CodeDeviceLimit, "device connection limit reached")
	}
	p.hostID = hostID
	p.deviceID = deviceID
	peers[p] = struct{}{}
	h.mu.Unlock()

	h.srv.sendPresenceToHost(host, deviceID, true)
	h.srv.emit(Event{Kind: EventDeviceConnected, HostID: hostID, DeviceID: deviceID})
	return nil
}

func (h *hub) removeDevice(p *peer) {
	key := deviceKey{host: p.hostID, device: p.deviceID}
	h.mu.Lock()
	peers, present := h.devices[key]
	if present {
		delete(peers, p)
		if len(peers) == 0 {
			delete(h.devices, key)
		}
	}
	var conns []*connState
	for conn, state := range h.conns {
		if conn.host == key.host && conn.device == key.device {
			conns = append(conns, state)
			delete(h.conns, conn)
		}
	}
	host := h.hosts[key.host]
	h.mu.Unlock()

	for _, state := range conns {
		h.srv.enqueueFrame(host, frame{T: "close", Conn: state.key.conn})
		h.srv.emit(Event{Kind: EventConnClosed, HostID: state.key.host, DeviceID: state.key.device, ConnID: state.key.conn})
	}
	if present && host != nil {
		h.srv.sendPresenceToHost(host, key.device, false)
	}
	h.srv.emit(Event{Kind: EventDeviceDisconnected, HostID: key.host, DeviceID: key.device})
}

func (h *hub) unregister(p *peer) {
	if h.srv.closed.Load() {
		return
	}
	if p.role == roleHost {
		h.removeHost(p)
		return
	}
	h.removeDevice(p)
}

func (h *hub) openConn(p *peer, connID string) *Error {
	key := deviceKey{host: p.hostID, device: p.deviceID}
	conn := connKey{host: p.hostID, device: p.deviceID, conn: connID}
	h.mu.Lock()
	if h.revoked[key] {
		h.mu.Unlock()
		return newError(CodeDeviceRevoked, "device revoked")
	}
	if _, ok := h.devices[key][p]; !ok {
		h.mu.Unlock()
		return newError(CodeUnknownDevice, "device not registered")
	}
	host := h.hosts[key.host]
	if host == nil {
		h.mu.Unlock()
		return newError(CodeHostOffline, "host not connected")
	}
	count := 0
	for existing := range h.conns {
		// Conn ids must be unique within a host: host data frames carry only
		// the conn id, so a cross-device collision would be unroutable.
		if existing.host == key.host && existing.conn == connID {
			h.mu.Unlock()
			return newError(CodeDuplicateConn, "conn already open")
		}
		if existing.host == key.host && existing.device == key.device {
			count++
		}
	}
	if count >= MaxConnsPerDevice {
		h.mu.Unlock()
		return newError(CodeConnLimit, "device conn limit reached")
	}
	state := &connState{key: conn, device: p, opened: true}
	h.conns[conn] = state
	h.mu.Unlock()

	if !h.srv.enqueueFrame(host, frame{T: "open", Conn: connID}) {
		h.dropConn(state)
		return newError(CodeQueueOverflow, "host queue full")
	}
	h.srv.emit(Event{Kind: EventConnOpened, HostID: key.host, DeviceID: key.device, ConnID: connID})
	return nil
}

func (h *hub) dropConn(state *connState) {
	h.mu.Lock()
	if h.conns[state.key] == state {
		delete(h.conns, state.key)
	}
	h.mu.Unlock()
}

func (h *hub) hostData(p *peer, connID, payload string) *Error {
	h.mu.Lock()
	var state *connState
	for key, candidate := range h.conns {
		if key.host == p.hostID && key.conn == connID {
			state = candidate
			break
		}
	}
	if state == nil {
		h.mu.Unlock()
		return newError(CodeUnknownConn, "unknown conn")
	}
	if !state.opened {
		h.mu.Unlock()
		return newError(CodeConnNotOpen, "conn not open")
	}
	device := state.device
	h.mu.Unlock()

	if !h.srv.enqueueFrame(device, frame{T: "data", Conn: connID, Payload: payload}) {
		h.dropConn(state)
		return newError(CodeQueueOverflow, "device queue full")
	}
	return nil
}

func (h *hub) deviceData(p *peer, connID, payload string) *Error {
	key := deviceKey{host: p.hostID, device: p.deviceID}
	conn := connKey{host: p.hostID, device: p.deviceID, conn: connID}
	h.mu.Lock()
	state := h.conns[conn]
	if state == nil {
		h.mu.Unlock()
		return newError(CodeUnknownConn, "unknown conn")
	}
	if !state.opened {
		h.mu.Unlock()
		return newError(CodeConnNotOpen, "conn not open")
	}
	host := h.hosts[key.host]
	if host == nil {
		delete(h.conns, conn)
		h.mu.Unlock()
		h.srv.enqueueFrame(p, frame{T: "close", Conn: connID})
		return newError(CodeHostOffline, "host not connected")
	}
	h.mu.Unlock()

	if !h.srv.enqueueFrame(host, frame{T: "data", Conn: connID, Payload: payload}) {
		h.dropConn(state)
		return newError(CodeQueueOverflow, "host queue full")
	}
	return nil
}

func (h *hub) closeFromDevice(p *peer, connID string) *Error {
	conn := connKey{host: p.hostID, device: p.deviceID, conn: connID}
	h.mu.Lock()
	state := h.conns[conn]
	if state == nil {
		h.mu.Unlock()
		return newError(CodeUnknownConn, "unknown conn")
	}
	delete(h.conns, conn)
	host := h.hosts[conn.host]
	h.mu.Unlock()

	h.srv.enqueueFrame(host, frame{T: "close", Conn: connID})
	h.srv.emit(Event{Kind: EventConnClosed, HostID: conn.host, DeviceID: conn.device, ConnID: connID})
	return nil
}

func (h *hub) closeFromHost(p *peer, connID string) *Error {
	h.mu.Lock()
	var state *connState
	for key, candidate := range h.conns {
		if key.host == p.hostID && key.conn == connID {
			state = candidate
			break
		}
	}
	if state == nil {
		h.mu.Unlock()
		return newError(CodeUnknownConn, "unknown conn")
	}
	delete(h.conns, state.key)
	h.mu.Unlock()

	h.srv.enqueueFrame(state.device, frame{T: "close", Conn: connID})
	h.srv.emit(Event{Kind: EventConnClosed, HostID: state.key.host, DeviceID: state.key.device, ConnID: connID})
	return nil
}

func (h *hub) revokeDevice(hostID, deviceID string) {
	key := deviceKey{host: hostID, device: deviceID}
	h.mu.Lock()
	h.revoked[key] = true
	peers := h.devices[key]
	delete(h.devices, key)
	var conns []*connState
	for conn, state := range h.conns {
		if conn.host == hostID && conn.device == deviceID {
			conns = append(conns, state)
			delete(h.conns, conn)
		}
	}
	host := h.hosts[hostID]
	h.mu.Unlock()

	for devicePeer := range peers {
		go devicePeer.fail(newFatal(CodeRevoked, "device revoked"))
	}
	for _, state := range conns {
		h.srv.emit(Event{Kind: EventConnClosed, HostID: hostID, DeviceID: deviceID, ConnID: state.key.conn})
	}
	if host != nil {
		h.srv.sendPresenceToHost(host, deviceID, false)
	}
	h.srv.emit(Event{Kind: EventDeviceRevoked, HostID: hostID, DeviceID: deviceID})
}

func (h *hub) repairDevice(hostID, deviceID string) {
	key := deviceKey{host: hostID, device: deviceID}
	h.mu.Lock()
	delete(h.revoked, key)
	h.mu.Unlock()
	h.srv.emit(Event{Kind: EventDeviceRepaired, HostID: hostID, DeviceID: deviceID})
}

func (h *hub) closeAll() {
	h.mu.Lock()
	var peers []*peer
	for _, host := range h.hosts {
		peers = append(peers, host)
	}
	for _, group := range h.devices {
		for devicePeer := range group {
			peers = append(peers, devicePeer)
		}
	}
	h.hosts = make(map[string]*peer)
	h.devices = make(map[deviceKey]map[*peer]struct{})
	h.conns = make(map[connKey]*connState)
	h.mu.Unlock()

	for _, p := range peers {
		p.terminate(CodeClosed)
	}
	h.srv.emit(Event{Kind: EventShutdown, Code: CodeClosed})
}
