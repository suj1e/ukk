import Foundation

enum JSONOutput {
    static func print<T: Encodable>(_ value: T) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        do {
            let data = try encoder.encode(value)
            FileHandle.standardOutput.write(data)
            FileHandle.standardOutput.write(Data("\n".utf8))
        } catch {
            throw HelperError.outputFailed(
                "could not encode metadata as JSON: \(error.localizedDescription)"
            )
        }
    }
}
