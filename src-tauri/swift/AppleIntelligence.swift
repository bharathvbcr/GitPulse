//  Apple Intelligence bridge for GitPulse.
//
//  GitPulse is Rust; Apple's Foundation Models framework is Swift-only. This
//  file is the whole of the Swift side: three C entry points, no state that
//  outlives a call, and JSON in both directions so the Rust side never has to
//  know a Swift type.
//
//  Everything here answers one question the host cannot answer for itself:
//  can this Mac run the on-device model right now, and if not, why. The
//  "why" matters more than the "no" — "Apple Intelligence is off in Settings"
//  is a thing the reader can fix, and "this Mac is not eligible" is not, and
//  a single `false` would have made them look the same.
//
//  Compiled by build.rs only when the macOS SDK actually has the framework.
//  When it does not, no symbol from this file exists and the Rust side says
//  "not compiled in" rather than "unavailable" — the two are different facts
//  and only one of them is about this Mac.

import Foundation

#if canImport(FoundationModels)
  import FoundationModels
#endif

// MARK: - Wire shapes

private struct Request: Decodable {
  /// "draft" writes both fields from notes; "improve" rewrites what exists.
  var kind: String
  /// Which of title/description the caller wants back.
  var fields: [String]
  var notes: String?
  var title: String?
  var description: String?
  /// Repository names, task type, labels — whatever helps the model be specific.
  var context: String?
}

private struct Reply: Encodable {
  var ok: Bool
  var code: String?
  var message: String?
  var title: String?
  var description: String?
  var rationale: String?
}

private struct Status: Encodable {
  /// True when this build can reach the framework at all.
  var compiled: Bool
  /// "available" | "unavailable" | "unsupported_os"
  var state: String
  /// Machine-readable reason when state is not "available".
  var reason: String?
  var detail: String
}

// MARK: - Generated shapes

#if canImport(FoundationModels)
  /// The shape the decoder enforces, and therefore the strongest instruction
  /// in the whole bridge.
  ///
  /// A `@Guide` outranks the session instructions in practice — the decoder is
  /// steering toward this text token by token, while the instructions are
  /// something the model merely read. Two of them used to pull against the
  /// rules the instructions state, and the guide was winning:
  ///
  ///  - **"at most 12 words"** is a budget a title cannot always pay. A note
  ///    whose point is `E42 in resolveWorktreeIdentity after 1.2.0` spends most
  ///    of it on the evidence, so the model bought the word count by rewording
  ///    the identifier — exactly what the instructions forbid. The bound that
  ///    is actually enforced is 300 characters, in `interpret` and again in the
  ///    store, so that is the one to state; brevity stays a preference.
  ///  - **"Two to five sentences"** is a floor, and a floor on a thin note is
  ///    an instruction to invent. "If the notes are thin, stay general rather
  ///    than guessing" cannot survive a schema that will not accept one
  ///    sentence.
  @available(macOS 26.0, *)
  @Generable
  private struct TaskText {
    @Guide(
      description:
        "An imperative one-line summary of the work, at most 300 characters. Aim for about a dozen words, but never buy that brevity by dropping or rewording an error code, identifier, path or version the notes rely on — go longer instead. No trailing period, no markdown, no quotes."
    )
    var title: String

    @Guide(
      description:
        "What to change and how to tell it worked, in plain prose: no markdown headings, no bullet characters. At most five sentences, and only as many as the notes actually support — one accurate sentence is better than three where the third is padding. Reproduce the notes' own error codes, identifiers, paths and commands exactly rather than describing them in your own words."
    )
    var description: String
  }
#endif

// MARK: - Entry points

/// JSON describing whether the on-device model can run right now.
@_cdecl("gitpulse_apple_intelligence_status")
public func gitpulseAppleIntelligenceStatus() -> UnsafeMutablePointer<CChar>? {
  #if canImport(FoundationModels)
    guard #available(macOS 26.0, *) else {
      return copyOut(
        Status(
          compiled: true, state: "unsupported_os", reason: "os_too_old",
          detail: "Apple Intelligence needs macOS 26 or later."))
    }
    switch SystemLanguageModel.default.availability {
    case .available:
      return copyOut(
        Status(
          compiled: true, state: "available", reason: nil,
          detail: "The on-device model is ready. Nothing leaves this Mac."))
    case .unavailable(.appleIntelligenceNotEnabled):
      return copyOut(
        Status(
          compiled: true, state: "unavailable", reason: "apple_intelligence_not_enabled",
          detail: "Turn on Apple Intelligence in System Settings to use it here."))
    case .unavailable(.deviceNotEligible):
      return copyOut(
        Status(
          compiled: true, state: "unavailable", reason: "device_not_eligible",
          detail: "This Mac cannot run Apple Intelligence."))
    case .unavailable(.modelNotReady):
      return copyOut(
        Status(
          compiled: true, state: "unavailable", reason: "model_not_ready",
          detail: "The on-device model is still downloading or preparing. Try again shortly."))
    case .unavailable(let other):
      // A reason added after this was written. Say that, rather than
      // inventing a cause or reporting a state that looks like a choice.
      return copyOut(
        Status(
          compiled: true, state: "unavailable", reason: "unrecognized",
          detail: "Apple Intelligence reported a reason this version does not recognize: \(other)."))
    }
  #else
    return copyOut(
      Status(
        compiled: false, state: "unsupported_os", reason: "not_compiled",
        detail: "This build was compiled without the Foundation Models framework."))
  #endif
}

/// Runs one on-device generation and returns its JSON reply.
///
/// Synchronous on purpose: the Rust caller is already on a worker thread and
/// has its own deadline. `timeoutMs` bounds the wait so a model that never
/// answers cannot hold that thread forever — the generation is abandoned, not
/// cancelled, because there is nothing to cancel through a C boundary.
@_cdecl("gitpulse_apple_intelligence_generate")
public func gitpulseAppleIntelligenceGenerate(
  _ requestJSON: UnsafePointer<CChar>?, _ timeoutMs: Int32
) -> UnsafeMutablePointer<CChar>? {
  guard let requestJSON else { return copyOut(fail("invalid_input", "No request was provided.")) }
  let raw = String(cString: requestJSON)
  guard let data = raw.data(using: .utf8),
    let request = try? JSONDecoder().decode(Request.self, from: data)
  else {
    return copyOut(fail("invalid_input", "The request could not be read."))
  }
  #if canImport(FoundationModels)
    guard #available(macOS 26.0, *) else {
      return copyOut(fail("unsupported_os", "Apple Intelligence needs macOS 26 or later."))
    }
    guard case .available = SystemLanguageModel.default.availability else {
      return copyOut(fail("unavailable", "The on-device model is not available right now."))
    }
    let box = ReplyBox()
    let done = DispatchSemaphore(value: 0)
    Task.detached(priority: .userInitiated) {
      let reply = await run(request)
      box.store(reply)
      done.signal()
    }
    let budget = max(1_000, min(Int(timeoutMs), 600_000))
    if done.wait(timeout: .now() + .milliseconds(budget)) == .timedOut {
      return copyOut(
        fail("timeout", "The on-device model did not answer within \(budget / 1000) seconds."))
    }
    return copyOut(box.take() ?? fail("worker_error", "The on-device model returned nothing."))
  #else
    _ = request
    return copyOut(
      fail("not_compiled", "This build was compiled without the Foundation Models framework."))
  #endif
}

/// Releases a string returned by either entry point above.
@_cdecl("gitpulse_apple_intelligence_free")
public func gitpulseAppleIntelligenceFree(_ pointer: UnsafeMutablePointer<CChar>?) {
  guard let pointer else { return }
  free(pointer)
}

// MARK: - Generation

#if canImport(FoundationModels)
  /// Carries one reply across the concurrency boundary.
  ///
  /// `@_cdecl` cannot be async, so the entry point blocks on a semaphore while
  /// a detached task produces the answer. The lock is what makes that handoff
  /// legal rather than merely usually-correct.
  private final class ReplyBox: @unchecked Sendable {
    private let lock = NSLock()
    private var value: Reply?
    func store(_ reply: Reply) {
      lock.lock()
      defer { lock.unlock() }
      value = reply
    }
    func take() -> Reply? {
      lock.lock()
      defer { lock.unlock() }
      return value
    }
  }

  @available(macOS 26.0, *)
  private func run(_ request: Request) async -> Reply {
    let wantsTitle = request.fields.contains("title")
    let wantsDescription = request.fields.contains("description")
    guard wantsTitle || wantsDescription else {
      return fail("invalid_input", "No fields were requested.")
    }
    let session = LanguageModelSession(instructions: instructions(for: request.kind))
    do {
      let response = try await session.respond(
        to: prompt(for: request, wantsTitle: wantsTitle, wantsDescription: wantsDescription),
        generating: TaskText.self,
        options: GenerationOptions(temperature: 0.4))
      let content = response.content
      return Reply(
        ok: true, code: nil, message: nil,
        title: wantsTitle ? clean(content.title, limit: 300) : nil,
        description: wantsDescription ? clean(content.description, limit: 65_536) : nil,
        rationale: "Written on this Mac by Apple Intelligence. No text left the device.")
    } catch let error as LanguageModelSession.GenerationError {
      return fail(code(for: error), describe(error))
    } catch {
      return fail("worker_error", error.localizedDescription)
    }
  }

  @available(macOS 26.0, *)
  private func code(for error: LanguageModelSession.GenerationError) -> String {
    switch error {
    case .assetsUnavailable: return "unavailable"
    case .exceededContextWindowSize: return "too_large"
    case .guardrailViolation: return "refused"
    case .refusal: return "refused"
    case .rateLimited: return "busy"
    case .concurrentRequests: return "busy"
    case .decodingFailure: return "worker_error"
    case .unsupportedGuide: return "worker_error"
    case .unsupportedLanguageOrLocale: return "unsupported_language"
    @unknown default: return "worker_error"
    }
  }

  @available(macOS 26.0, *)
  private func describe(_ error: LanguageModelSession.GenerationError) -> String {
    switch error {
    case .assetsUnavailable:
      return "The on-device model's assets are not available yet."
    case .exceededContextWindowSize:
      return "This task is too long for the on-device model. Shorten the notes and try again."
    case .guardrailViolation, .refusal:
      return "Apple Intelligence declined to write this. Rephrase the notes and try again."
    case .rateLimited, .concurrentRequests:
      return "Apple Intelligence is busy. Try again in a moment."
    case .unsupportedLanguageOrLocale:
      return "Apple Intelligence does not support this language yet."
    default:
      return error.errorDescription ?? "The on-device model could not finish."
    }
  }

  /// The rules for one drafting kind, all of them built on `shared`.
  ///
  /// Preserving the author's meaning belongs in `shared` and not in a branch.
  /// It used to sit only on `improve`, which left the two kinds that need it
  /// most without it: `draft` writes the first version of a task from raw
  /// notes, and `extract` is under explicit instruction to leave material out
  /// — the one place a model will happily drop the error code the note was
  /// written to record. Each branch may only *add* to `shared`; a branch that
  /// replaced it would take the rule back out for that kind.
  private func instructions(for kind: String) -> String {
    let shared = """
      You turn engineering notes into one task's title and description for a \
      developer's own task board. Write plain prose in the notes' own language. \
      Preserve the author's intent and message: keep their meaning and their \
      terminology, and reproduce exactly any error code, identifier, path, \
      version, command or quoted text they wrote. Never invent file names, \
      ticket numbers, people, dates, or decisions that the notes do not \
      contain. If the notes are thin, stay general rather than guessing. Do \
      not address the reader, do not add a preamble, and do not explain what \
      you are doing.
      """
    switch kind {
    case "improve":
      return shared
        + " Make the wording clearer and more specific without adding new claims."
    case "extract":
      return shared
        + " The notes are raw and unstructured. Pull out the single piece of work they describe and leave the rest out, keeping that piece in the author's own words."
    case "commit_subject":
      return shared
        + " The notes are a factual brief of a staged git change that the application has already classified. The title is one commit subject: imperative, at most 72 characters, no trailing period. When the current title begins with a type prefix such as docs: or feat(scope):, keep that prefix character for character and rephrase only the words after it. When it has no type prefix, add one only from feat, fix, refactor, perf, style, test, docs, build, ci, chore or revert, and only when the notes support it. Do not add a body, a trailer, or an issue number."
    default:
      return shared
    }
  }

  /// The request body, ending in the one thing being asked for.
  ///
  /// A field the caller did not request is still *shown*, because it is
  /// context the other field has to agree with — but the model was never told
  /// it is off limits. Asked for a description with a title already present,
  /// it would restate the title as the opening sentence; asked for a title
  /// with a description present, it would quietly propose a better
  /// description too. The decoder fills both properties either way and the
  /// bridge blanks the unwanted one, so the waste was invisible from here and
  /// showed up as a worse answer for the field that *was* asked for.
  ///
  /// The clause is conditional on the other field actually being in the
  /// prompt. Telling a model to leave a title alone when no title was supplied
  /// is a sentence about nothing, and it reads as though one exists.
  private func prompt(for request: Request, wantsTitle: Bool, wantsDescription: Bool) -> String {
    var parts: [String] = []
    if let context = trimmed(request.context) { parts.append("Context: \(context)") }
    let currentTitle = trimmed(request.title)
    let currentDescription = trimmed(request.description)
    if let title = currentTitle { parts.append("Current title: \(title)") }
    if let description = currentDescription {
      parts.append("Current description:\n\(description)")
    }
    if let notes = trimmed(request.notes) { parts.append("Notes:\n\(notes)") }
    let asked: String
    switch (wantsTitle, wantsDescription) {
    case (true, true):
      asked = "Write both the title and the description."
    case (true, false):
      asked =
        currentDescription == nil
        ? "Write the title only."
        : "Write the title only. The current description stays exactly as it is — do not rewrite it, and do not repeat it in the title."
    default:
      asked =
        currentTitle == nil
        ? "Write the description only."
        : "Write the description only. The current title stays exactly as it is — do not rewrite it, and do not restate it as the description's first sentence."
    }
    parts.append(asked)
    return parts.joined(separator: "\n\n")
  }

  private func trimmed(_ value: String?) -> String? {
    guard let value else { return nil }
    let text = value.trimmingCharacters(in: .whitespacesAndNewlines)
    return text.isEmpty ? nil : text
  }

  /// Strips the decorations a chat model adds out of habit.
  ///
  /// The schema asks for a bare title, and the model mostly obliges — but a
  /// leading "Title: " or a wrapping pair of quotes arrives often enough that
  /// letting it through would put it in the reader's task.
  private func clean(_ value: String, limit: Int) -> String {
    var text = value.trimmingCharacters(in: .whitespacesAndNewlines)
    for prefix in ["Title:", "Description:", "title:", "description:"] where text.hasPrefix(prefix) {
      text = String(text.dropFirst(prefix.count)).trimmingCharacters(in: .whitespacesAndNewlines)
    }
    if text.count > 1, text.hasPrefix("\""), text.hasSuffix("\"") {
      text = String(text.dropFirst().dropLast()).trimmingCharacters(in: .whitespacesAndNewlines)
    }
    return cut(text, toScalars: limit)
  }

  /// Bounds a string by the unit everything downstream counts in.
  ///
  /// `String.prefix` counts *grapheme clusters*; `interpret` and the store both
  /// count `chars()`, which is Unicode scalars. Those are not the same number —
  /// one family emoji is 1 grapheme and 5 scalars — so `prefix(300)` could hand
  /// back 1500 scalars and the store would refuse the proposal after the model
  /// had already run. In the other direction, on plain ASCII the clip landed
  /// exactly on 300 and made the downstream refusal unreachable, so the check
  /// that was supposed to catch an over-long title could never fire.
  ///
  /// Whole characters, counted in scalars: the result satisfies the bound the
  /// store actually enforces, and never leaves a combining mark or a ZWJ
  /// sequence severed from the character it belonged to.
  private func cut(_ text: String, toScalars limit: Int) -> String {
    guard text.unicodeScalars.count > limit else { return text }
    var kept = ""
    var used = 0
    for character in text {
      let width = character.unicodeScalars.count
      if used + width > limit { break }
      kept.append(character)
      used += width
    }
    return kept
  }
#endif

// MARK: - Plumbing

private func fail(_ code: String, _ message: String) -> Reply {
  Reply(ok: false, code: code, message: message, title: nil, description: nil, rationale: nil)
}

/// Encodes a value and hands the bytes to the caller, who must free them.
///
/// `strdup` rather than a Swift allocation: the Rust side frees through
/// `gitpulse_apple_intelligence_free`, which calls `free`, and pairing those
/// two is the only allocation contract that crosses this boundary.
private func copyOut<T: Encodable>(_ value: T) -> UnsafeMutablePointer<CChar>? {
  let json =
    (try? JSONEncoder().encode(value)).flatMap { String(data: $0, encoding: .utf8) }
    ?? #"{"ok":false,"code":"worker_error","message":"The reply could not be encoded."}"#
  return strdup(json)
}
