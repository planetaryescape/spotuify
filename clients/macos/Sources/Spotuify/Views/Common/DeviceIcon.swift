enum DeviceIcon {
    static func symbol(for kind: String) -> String {
        switch kind.lowercased() {
        case "computer": "laptopcomputer"
        case "smartphone": "iphone"
        case "speaker": "hifispeaker.fill"
        case "tv", "castvideo": "tv"
        case "avr", "stb": "av.remote"
        case "automobile": "car.fill"
        default: "hifispeaker"
        }
    }
}
