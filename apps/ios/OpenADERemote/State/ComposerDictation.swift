import AVFoundation
import Observation
import Speech

/// Dictation only edits the draft. Audio stays on the iPhone and is never sent to the host.
@Observable @MainActor
final class ComposerDictation {
    private(set) var transcript = ""
    private(set) var isRecording = false
    private(set) var isStarting = false
    var error: String?

    @ObservationIgnored private var engine: AVAudioEngine?
    @ObservationIgnored private var request: SFSpeechAudioBufferRecognitionRequest?
    @ObservationIgnored private var recognition: SFSpeechRecognitionTask?
    @ObservationIgnored private var recognizer: SFSpeechRecognizer?
    @ObservationIgnored private var interruption: NSObjectProtocol?
    @ObservationIgnored private var generation = UUID()
    @ObservationIgnored private var tapInstalled = false

    func start() async {
        guard !isRecording, !isStarting, !Task.isCancelled else { return }
        let generation = UUID()
        self.generation = generation
        isStarting = true
        error = nil
        transcript = ""
        defer { if self.generation == generation { isStarting = false } }

        let speechAllowed = await withCheckedContinuation { continuation in
            SFSpeechRecognizer.requestAuthorization { continuation.resume(returning: $0 == .authorized) }
        }
        guard self.generation == generation, !Task.isCancelled else { return }
        guard speechAllowed else {
            error = "Permití el reconocimiento de voz en Ajustes para dictar un mensaje."
            return
        }
        let microphoneAllowed = await withCheckedContinuation { continuation in
            AVAudioApplication.requestRecordPermission { continuation.resume(returning: $0) }
        }
        guard self.generation == generation, !Task.isCancelled else { return }
        guard microphoneAllowed else {
            error = "Permití el micrófono en Ajustes para dictar un mensaje."
            return
        }
        guard let recognizer = SFSpeechRecognizer(locale: .current), recognizer.isAvailable,
              recognizer.supportsOnDeviceRecognition else {
            error = "El dictado en este iPhone no está disponible para el idioma actual. Podés escribir o usar el dictado del teclado."
            return
        }

        do {
            self.recognizer = recognizer
            let audioSession = AVAudioSession.sharedInstance()
            try audioSession.setCategory(.record, mode: .measurement, options: .duckOthers)
            try audioSession.setActive(true, options: .notifyOthersOnDeactivation)
            let engine = AVAudioEngine()
            self.engine = engine
            let request = SFSpeechAudioBufferRecognitionRequest()
            request.requiresOnDeviceRecognition = true
            request.shouldReportPartialResults = true
            request.addsPunctuation = true
            self.request = request
            let input = engine.inputNode
            let format = input.outputFormat(forBus: 0)
            guard format.sampleRate > 0, format.channelCount > 0 else {
                throw DictationFailure.noInput
            }
            input.installTap(onBus: 0, bufferSize: 1024, format: format) { buffer, _ in
                request.append(buffer)
            }
            tapInstalled = true
            recognition = recognizer.recognitionTask(with: request) { [weak self] result, failure in
                let text = result?.bestTranscription.formattedString
                let finished = result?.isFinal == true
                let failed = failure != nil
                Task { @MainActor in
                    guard let self, self.generation == generation else { return }
                    if let text { self.transcript = text }
                    if finished || failed {
                        if failed, self.transcript.isEmpty { self.error = "No pude transcribir el audio. Intentá dictar otra vez." }
                        self.stop()
                    }
                }
            }
            interruption = NotificationCenter.default.addObserver(forName: AVAudioSession.interruptionNotification, object: nil, queue: .main) { [weak self] _ in
                Task { @MainActor in self?.stop() }
            }
            engine.prepare()
            try engine.start()
            isRecording = true
        } catch {
            stop()
            self.error = "No pude iniciar el micrófono. Revisá que otra app no lo esté utilizando."
        }
    }

    func stop() {
        generation = UUID()
        isStarting = false
        let wasRecording = engine != nil
        engine?.stop()
        if tapInstalled { engine?.inputNode.removeTap(onBus: 0) }
        tapInstalled = false
        request?.endAudio()
        recognition?.cancel()
        engine = nil
        request = nil
        recognition = nil
        recognizer = nil
        if let interruption { NotificationCenter.default.removeObserver(interruption) }
        interruption = nil
        isRecording = false
        if wasRecording { try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation) }
    }
}

private enum DictationFailure: Error { case noInput }
