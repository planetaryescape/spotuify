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

    /// Recover just the correlation `id` and payload `type` from a frame whose
    /// full decode failed, so a stranded pending request can be failed fast
    /// instead of waiting out its timeout.
    static func decodeFrameEnvelope(_ data: Data) throws -> FrameEnvelope {
        try JSONDecoder().decode(FrameEnvelope.self, from: data)
    }

    static func requestFingerprint(_ request: DaemonRequest) throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return try encoder.encode(request)
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

/// A frame stripped to its correlation `id` and payload `type`. Used only to
/// triage a frame whose full decode failed; both fields decode leniently so
/// this never throws on the same malformed payload.
struct FrameEnvelope: Decodable {
    let id: UInt64
    let type: String?

    private enum RootKeys: String, CodingKey { case id, payload }
    private enum PayloadKeys: String, CodingKey { case type }

    init(from decoder: Decoder) throws {
        let root = try decoder.container(keyedBy: RootKeys.self)
        id = try root.decode(UInt64.self, forKey: .id)
        let payload = try? root.nestedContainer(keyedBy: PayloadKeys.self, forKey: .payload)
        type = try? payload?.decode(String.self, forKey: .type)
    }
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

/// A daemon request paired with the stable retry key used by every attempt.
///
/// When using the prepared-request overload, retain this value and pass the
/// same instance after a timeout; that overload does not retry automatically.
/// Constructing a new prepared request generates a new mutation ID and
/// represents a new write. The source-compatible `DaemonRequest` overload
/// retains uncertain attempts internally for its existing callers.
public struct PreparedDaemonRequest: Sendable {
    public let request: DaemonRequest
    public let mutationID: UUID?

    public init(_ request: DaemonRequest, mutationID: UUID? = nil) {
        self.request = request
        self.mutationID = request.requiresMutationId
            ? (mutationID ?? MutationUUID.v7())
            : nil
    }
}

struct PreparedDaemonAttempt {
    let fingerprint: Data?
    let prepared: PreparedDaemonRequest
    let wasUncertain: Bool
}

enum MutationAttemptDisposition: Equatable {
    /// The daemon returned a success or typed error for this mutation ID.
    case definitive
    /// The request may have reached the daemon, but its response was lost.
    case uncertain
    /// This attempt failed before it could be sent.
    case notSent
}

/// Retains only mutation attempts whose response was lost. Each retained ID is
/// tracked independently so a definitive response for a concurrent identical
/// request cannot discard another attempt's retry key.
///
/// Reuse is bounded by `retentionTTL`: a lost response is retried within
/// seconds, so a payload-identical request that arrives minutes later is a new
/// user action, not the same write. Without the bound a stale retry key could
/// be reused days later, making the daemon's durable dedup replay the old
/// receipt instead of executing (e.g. a re-queue silently does nothing while
/// the toast claims success). Expired entries are discarded and mint fresh IDs.
struct MutationRetryCache {
    static let capacity = 128
    /// How long a lost-response retry key stays reusable. Long enough to cover
    /// a reconnect-and-retry, short enough that a later identical request is a
    /// fresh write rather than a replay of a stale receipt.
    static let retentionTTL: TimeInterval = 120

    private struct RetainedKey: Equatable {
        let fingerprint: Data
        let mutationID: UUID
    }

    private struct RetainedEntry {
        let prepared: PreparedDaemonRequest
        let retainedAt: Date
    }

    private var uncertain: [Data: [RetainedEntry]] = [:]
    private var oldestFirst: [RetainedKey] = []
    private let now: () -> Date

    init(now: @escaping () -> Date = { Date() }) {
        self.now = now
    }

    var count: Int { oldestFirst.count }

    mutating func attempt(for request: DaemonRequest) throws -> PreparedDaemonAttempt {
        guard request.requiresMutationId else {
            return PreparedDaemonAttempt(
                fingerprint: nil,
                prepared: PreparedDaemonRequest(request),
                wasUncertain: false)
        }
        let fingerprint = try Wire.requestFingerprint(request)
        pruneExpired(fingerprint: fingerprint)
        let prepared: PreparedDaemonRequest
        let wasUncertain: Bool
        if var retained = uncertain[fingerprint], !retained.isEmpty {
            prepared = retained.removeFirst().prepared
            wasUncertain = true
            if retained.isEmpty {
                uncertain.removeValue(forKey: fingerprint)
            } else {
                uncertain[fingerprint] = retained
            }
            if let mutationID = prepared.mutationID {
                removeOldestEntry(fingerprint: fingerprint, mutationID: mutationID)
            }
        } else {
            prepared = PreparedDaemonRequest(request)
            wasUncertain = false
        }
        return PreparedDaemonAttempt(
            fingerprint: fingerprint,
            prepared: prepared,
            wasUncertain: wasUncertain)
    }

    /// Drop retained keys for this fingerprint that have aged past the TTL. An
    /// expired key is abandoned, so the next attempt mints a fresh mutation ID.
    private mutating func pruneExpired(fingerprint: Data) {
        guard let retained = uncertain[fingerprint] else { return }
        let cutoff = now().addingTimeInterval(-Self.retentionTTL)
        let fresh = retained.filter { $0.retainedAt >= cutoff }
        guard fresh.count != retained.count else { return }
        let expiredIDs = Set(
            retained.filter { $0.retainedAt < cutoff }.compactMap(\.prepared.mutationID))
        if fresh.isEmpty {
            uncertain.removeValue(forKey: fingerprint)
        } else {
            uncertain[fingerprint] = fresh
        }
        oldestFirst.removeAll {
            $0.fingerprint == fingerprint && expiredIDs.contains($0.mutationID)
        }
    }

    mutating func finish(_ attempt: PreparedDaemonAttempt, uncertainOutcome: Bool) {
        finish(
            attempt,
            disposition: uncertainOutcome ? .uncertain : .definitive)
    }

    mutating func finish(
        _ attempt: PreparedDaemonAttempt,
        disposition: MutationAttemptDisposition
    ) {
        guard let fingerprint = attempt.fingerprint,
              let mutationID = attempt.prepared.mutationID
        else { return }
        removeRetained(fingerprint: fingerprint, mutationID: mutationID)
        if disposition == .uncertain || (disposition == .notSent && attempt.wasUncertain) {
            uncertain[fingerprint, default: []].append(
                RetainedEntry(prepared: attempt.prepared, retainedAt: now()))
            oldestFirst.append(
                RetainedKey(fingerprint: fingerprint, mutationID: mutationID))
            while oldestFirst.count > Self.capacity {
                let oldest = oldestFirst.removeFirst()
                removeRetained(
                    fingerprint: oldest.fingerprint,
                    mutationID: oldest.mutationID,
                    removeOrderEntry: false)
            }
        }
    }

    private mutating func removeRetained(
        fingerprint: Data,
        mutationID: UUID,
        removeOrderEntry: Bool = true
    ) {
        if var retained = uncertain[fingerprint] {
            retained.removeAll { $0.prepared.mutationID == mutationID }
            if retained.isEmpty {
                uncertain.removeValue(forKey: fingerprint)
            } else {
                uncertain[fingerprint] = retained
            }
        }
        if removeOrderEntry {
            removeOldestEntry(fingerprint: fingerprint, mutationID: mutationID)
        }
    }

    private mutating func removeOldestEntry(fingerprint: Data, mutationID: UUID) {
        oldestFirst.removeAll {
            $0.fingerprint == fingerprint && $0.mutationID == mutationID
        }
    }
}

/// One outbound frame: `{ "id": u64, "payload": { "type": "Request", "cmd": ... } }`.
struct OutboundMessage: Encodable {
    let id: UInt64
    let request: DaemonRequest
    let mutationId: UUID?

    init(id: UInt64, request: DaemonRequest, mutationId: UUID? = nil) {
        self.init(id: id, prepared: PreparedDaemonRequest(request, mutationID: mutationId))
    }

    init(id: UInt64, prepared: PreparedDaemonRequest) {
        self.id = id
        self.request = prepared.request
        self.mutationId = prepared.mutationID
    }

    enum CodingKeys: String, CodingKey { case id, payload, mutationId = "mutation_id" }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encodeIfPresent(mutationId, forKey: .mutationId)
        // DaemonRequest.encode writes {"type":"Request","cmd":...,<fields>}
        try container.encode(request, forKey: .payload)
    }
}

private enum MutationUUID {
    static func v7(now: Date = Date()) -> UUID {
        let milliseconds = UInt64(max(0, now.timeIntervalSince1970 * 1_000))
        var bytes = [UInt8](repeating: 0, count: 16)
        for index in bytes.indices {
            bytes[index] = UInt8.random(in: .min ... .max)
        }
        bytes[0] = UInt8((milliseconds >> 40) & 0xff)
        bytes[1] = UInt8((milliseconds >> 32) & 0xff)
        bytes[2] = UInt8((milliseconds >> 24) & 0xff)
        bytes[3] = UInt8((milliseconds >> 16) & 0xff)
        bytes[4] = UInt8((milliseconds >> 8) & 0xff)
        bytes[5] = UInt8(milliseconds & 0xff)
        bytes[6] = 0x70 | (bytes[6] & 0x0f)
        bytes[8] = 0x80 | (bytes[8] & 0x3f)
        return UUID(uuid: (
            bytes[0], bytes[1], bytes[2], bytes[3],
            bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11],
            bytes[12], bytes[13], bytes[14], bytes[15]
        ))
    }
}
