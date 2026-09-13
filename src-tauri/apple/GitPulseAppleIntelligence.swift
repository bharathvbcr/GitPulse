// On-device task drafting through Apple's Foundation Models.
//
// Exposed to Rust as three C entry points. The design constraints that shaped
// them, in order of how much they cost to discover:
//
//   - `@_cdecl` functions cannot be `async`, and every FoundationModels
//     generation call is. A detached `Task` does the work and the C thread waits
//     on a `DispatchSemaphore` with a bounded timeout, handing the reply across
//     through a lock-guarded box. An unbounded wait here would hang the caller.
//   - Returned strings are allocated with `strdup` and must be released through
//     the matching `gitpulse_apple_string_free`. Rust never frees them itself.
//   - Everything crosses as JSON. One text channel keeps the ABI stable as the
//     proposal shape changes, and means a Swift-side failure arrives as data
//     rather than as a trap across the language boundary.
//
// The framework needs macOS 26; this file is compiled with a lower deployment
// target, so every use sits behind `if #available`. That path is the
// "unsupported OS" answer, which callers must keep distinct from "the framework
// is present and says no".

import Foundation

#if canImport(FoundationModels)
import FoundationModels
#endif

/// A task draft the model fills in.
///
/// `@Generable` expands fine under the Xcode toolchain, so a hand-built
/// `DynamicGenerationSchema` is not needed. Descriptions are part of the prompt
/// the model sees, not documentation: they are what keeps the output the shape
/// GitPulse's task editor expects.
#if canImport(FoundationModels)
@available(macOS 26.0, *)
@Generable
struct TaskDraft {
    @Guide(description: "A short imperative task title, at most 80 characters, with no trailing period.")
    var title: String

    @Guide(description: "Two to four sentences describing what to do and why, in plain prose with no markdown headings.")
    var description: String

    @Guide(description: "One sentence naming the evidence in the provided context that this draft is based on.")
    var rationale: String
}
#endif

/// Carries a result from the detached task back to the waiting C thread.
private final class ReplyBox: @unchecked Sendable {
    private let lock = NSLock()
    private var value: String?

    func set(_ next: String) {
        lock.lock()
        defer { lock.unlock() }
        // First writer wins: a late completion after a timeout must not
        // overwrite the timeout reply the caller has already been handed.
        if value == nil { value = next }
    }

    func take() -> String? {
        lock.lock()
        defer { lock.unlock() }
        return value
    }
}

/// JSON-escapes `text` for embedding in the replies below.
private func jsonString(_ text: String) -> String {
    guard let data = try? JSONEncoder().encode(text),
          let encoded = String(data: data, encoding: .utf8)
    else {
        return "\"\""
    }
    return encoded
}

private func failureReply(_ message: String) -> String {
    "{\"ok\":false,\"failure\":\(jsonString(message))}"
}

/// Duplicates `text` into a C string the caller owns.
private func cString(_ text: String) -> UnsafeMutablePointer<CChar>? {
    text.withCString { strdup($0) }
}

/// Releases a string returned by any entry point here.
@_cdecl("gitpulse_apple_string_free")
public func gitpulse_apple_string_free(_ pointer: UnsafeMutablePointer<CChar>?) {
    guard let pointer else { return }
    free(pointer)
}

/// Reports whether on-device generation can run right now.
///
/// The reasons are returned verbatim rather than flattened into a boolean,
/// because they call for different actions: an ineligible device cannot be
/// fixed, Apple Intelligence being switched off can be fixed in Settings, and a
/// model still downloading fixes itself.
@_cdecl("gitpulse_apple_availability")
public func gitpulse_apple_availability() -> UnsafeMutablePointer<CChar>? {
#if canImport(FoundationModels)
    if #available(macOS 26.0, *) {
        switch SystemLanguageModel.default.availability {
        case .available:
            return cString("{\"available\":true,\"reason\":null}")
        case .unavailable(let reason):
            let name: String
            switch reason {
            case .deviceNotEligible: name = "device_not_eligible"
            case .appleIntelligenceNotEnabled: name = "apple_intelligence_not_enabled"
            case .modelNotReady: name = "model_not_ready"
            @unknown default: name = "unknown"
            }
            return cString("{\"available\":false,\"reason\":\(jsonString(name))}")
        @unknown default:
            return cString("{\"available\":false,\"reason\":\"unknown\"}")
        }
    }
    return cString("{\"available\":false,\"reason\":\"os_too_old\"}")
#else
    return cString("{\"available\":false,\"reason\":\"framework_missing\"}")
#endif
}

/// Generates one task draft, blocking for at most `timeoutMilliseconds`.
///
/// The timeout is the caller's, and must stay below the store's pending
/// enhancement lease — a generation that finishes after the lease expires is
/// refused on arrival, so the model call would be spent for nothing.
@_cdecl("gitpulse_apple_generate")
public func gitpulse_apple_generate(
    _ instructions: UnsafePointer<CChar>?,
    _ prompt: UnsafePointer<CChar>?,
    _ timeoutMilliseconds: Int32
) -> UnsafeMutablePointer<CChar>? {
#if canImport(FoundationModels)
    guard let prompt, let instructions else {
        return cString(failureReply("The bridge received no prompt."))
    }
    let promptText = String(cString: prompt)
    let instructionText = String(cString: instructions)
    guard !promptText.isEmpty else {
        return cString(failureReply("The bridge received an empty prompt."))
    }
    guard timeoutMilliseconds > 0 else {
        return cString(failureReply("The bridge received a non-positive timeout."))
    }

    if #available(macOS 26.0, *) {
        let box = ReplyBox()
        let ready = DispatchSemaphore(value: 0)
        Task.detached {
            do {
                let session = LanguageModelSession(instructions: instructionText)
                let response = try await session.respond(to: promptText, generating: TaskDraft.self)
                let draft = response.content
                box.set(
                    "{\"ok\":true,\"title\":\(jsonString(draft.title)),"
                        + "\"description\":\(jsonString(draft.description)),"
                        + "\"rationale\":\(jsonString(draft.rationale))}"
                )
            } catch {
                box.set(failureReply("On-device generation failed: \(error.localizedDescription)"))
            }
            ready.signal()
        }

        let deadline = DispatchTime.now() + .milliseconds(Int(timeoutMilliseconds))
        if ready.wait(timeout: deadline) == .timedOut {
            // Claim the box so a later completion cannot contradict this reply.
            box.set(failureReply("On-device generation exceeded its \(timeoutMilliseconds) ms budget."))
        }
        return cString(box.take() ?? failureReply("On-device generation produced no reply."))
    }
    return cString(failureReply("On-device generation requires macOS 26 or later."))
#else
    return cString(failureReply("This build has no Apple Foundation Models support."))
#endif
}
