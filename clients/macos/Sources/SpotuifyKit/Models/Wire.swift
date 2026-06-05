import Foundation

/// A coding key built from an arbitrary string — used to write/read the
/// daemon's flattened, dynamically-keyed JSON (e.g. `cmd`, `play-uri`).
struct AnyKey: CodingKey {
    let stringValue: String
    var intValue: Int? { nil }
    init(_ stringValue: String) { self.stringValue = stringValue }
    init?(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { nil }
}

/// JSON (de)serialization for the IPC wire. Plain coders with explicit
/// CodingKeys everywhere — no key strategy — so each key in the models maps
/// exactly to the daemon's JSON.
enum Wire {
    static func decodeMessage(_ data: Data) throws -> IpcMessage {
        try JSONDecoder().decode(IpcMessage.self, from: data)
    }

    static func encodeOutbound(_ message: OutboundMessage) throws -> Data {
        try JSONEncoder().encode(message)
    }
}

// MARK: - Inbound envelope

/// One inbound frame: `{ "id": u64, "source"?: string, "payload": {...} }`.
/// The client only ever receives `Response` and `Event` payloads.
struct IpcMessage: Decodable {
    let id: UInt64
    let payload: InboundPayload

    enum CodingKeys: String, CodingKey { case id, payload }
}

/// `payload` is internally tagged by `type`.
enum InboundPayload: Decodable {
    case response(ResponsePayload)
    case event(DaemonEvent)
    case other(type: String)

    private enum CodingKeys: String, CodingKey { case type }

    init(from decoder: Decoder) throws {
        let type = try decoder.container(keyedBy: CodingKeys.self).decode(String.self, forKey: .type)
        switch type {
        case "Response": self = .response(try ResponsePayload(from: decoder))
        case "Event": self = .event(try DaemonEvent(from: decoder))
        default: self = .other(type: type)
        }
    }
}

// MARK: - Outbound envelope

/// One outbound frame: `{ "id": u64, "payload": { "type": "Request", "cmd": ... } }`.
struct OutboundMessage: Encodable {
    let id: UInt64
    let request: DaemonRequest

    enum CodingKeys: String, CodingKey { case id, payload }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        // DaemonRequest.encode writes {"type":"Request","cmd":...,<fields>}
        try container.encode(request, forKey: .payload)
    }
}
