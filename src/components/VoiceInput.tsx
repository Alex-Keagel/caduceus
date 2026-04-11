import { useEffect, useRef, useState } from "react";

interface VoiceInputProps {
  onTranscript: (text: string) => void;
  enabled: boolean;
  language?: string;
}

function formatDuration(ms: number): string {
  const seconds = Math.max(ms / 1000, 0);
  return `${seconds.toFixed(seconds >= 10 ? 0 : 1)}s`;
}

export default function VoiceInput({ onTranscript, enabled, language = "en" }: VoiceInputProps) {
  const [isRecording, setIsRecording] = useState(false);
  const [status, setStatus] = useState("Ready to record");
  const [error, setError] = useState<string | null>(null);

  const canvasRef = useRef<HTMLCanvasElement>(null);
  const dotRef = useRef<HTMLSpanElement>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const audioContextRef = useRef<AudioContext | null>(null);
  const analyserRef = useRef<AnalyserNode | null>(null);
  const sourceRef = useRef<MediaStreamAudioSourceNode | null>(null);
  const rafRef = useRef<number>(0);
  const waveformBufferRef = useRef<Uint8Array<ArrayBuffer> | null>(null);
  const startedAtRef = useRef<number | null>(null);
  const enabledRef = useRef(enabled);
  const startAttemptRef = useRef(0);
  const mountedRef = useRef(true);

  const clearRecordingResources = async () => {
    cancelAnimationFrame(rafRef.current);
    rafRef.current = 0;

    sourceRef.current?.disconnect();
    analyserRef.current?.disconnect();

    streamRef.current?.getTracks().forEach((track) => track.stop());

    if (audioContextRef.current && audioContextRef.current.state !== "closed") {
      await audioContextRef.current.close();
    }

    const duration = startedAtRef.current ? Date.now() - startedAtRef.current : 0;

    sourceRef.current = null;
    analyserRef.current = null;
    streamRef.current = null;
    audioContextRef.current = null;
    waveformBufferRef.current = null;
    startedAtRef.current = null;

    return duration;
  };

  useEffect(() => {
    const dot = dotRef.current;
    if (!dot || typeof dot.animate !== "function" || !isRecording) return;

    const animation = dot.animate(
      [{ opacity: 0.3, transform: "scale(0.9)" }, { opacity: 1, transform: "scale(1.2)" }, { opacity: 0.3, transform: "scale(0.9)" }],
      { duration: 900, iterations: Number.POSITIVE_INFINITY, easing: "ease-in-out" }
    );

    return () => {
      animation.cancel();
    };
  }, [isRecording]);

  useEffect(() => {
    enabledRef.current = enabled;
  }, [enabled]);

  useEffect(() => {
    if (enabled) return;
    startAttemptRef.current += 1;
    void stopRecording(false, "Voice input disabled");
  }, [enabled]);

  useEffect(() => {
    return () => {
      mountedRef.current = false;
      void clearRecordingResources();
    };
  }, []);

  const stopRecording = async (emitTranscript = true, nextStatus?: string) => {
    const duration = await clearRecordingResources();

    if (!mountedRef.current) return;

    setIsRecording(false);
    setStatus(nextStatus ?? (enabled ? "Ready to record" : "Voice input disabled"));

    if (emitTranscript && enabledRef.current && duration > 250) {
      const transcript = `Simulated ${language} transcript captured from a ${formatDuration(duration)} voice note.`;
      onTranscript(transcript);
      setStatus(`Transcript ready · ${formatDuration(duration)}`);
    }
  };

  const drawWaveform = () => {
    const canvas = canvasRef.current;
    const analyser = analyserRef.current;
    if (!canvas || !analyser) return;

    const rect = canvas.getBoundingClientRect();
    const width = Math.max(Math.floor(rect.width), 1);
    const height = Math.max(Math.floor(rect.height), 1);
    const dpr = window.devicePixelRatio || 1;

    if (canvas.width !== width * dpr || canvas.height !== height * dpr) {
      canvas.width = width * dpr;
      canvas.height = height * dpr;
    }

    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, width, height);

    const buffer =
      waveformBufferRef.current && waveformBufferRef.current.length === analyser.frequencyBinCount
        ? waveformBufferRef.current
        : new Uint8Array(new ArrayBuffer(analyser.frequencyBinCount));
    waveformBufferRef.current = buffer;
    analyser.getByteTimeDomainData(buffer);

    ctx.fillStyle = "rgba(15, 23, 42, 0.92)";
    ctx.fillRect(0, 0, width, height);

    ctx.strokeStyle = "rgba(239, 68, 68, 0.95)";
    ctx.lineWidth = 2;
    ctx.beginPath();

    buffer.forEach((sample, index) => {
      const x = (index / (buffer.length - 1 || 1)) * width;
      const y = (sample / 255) * height;
      if (index === 0) ctx.moveTo(x, y);
      else ctx.lineTo(x, y);
    });

    ctx.stroke();

    rafRef.current = requestAnimationFrame(drawWaveform);
  };

  const startRecording = async () => {
    if (!enabled || isRecording) return;
    const startAttempt = ++startAttemptRef.current;

    try {
      if (!navigator.mediaDevices?.getUserMedia) {
        throw new Error("Microphone capture is unavailable in this environment.");
      }

      setError(null);
      setStatus("Requesting microphone access…");

      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const context = new AudioContext();
      await context.resume();

      const analyser = context.createAnalyser();
      analyser.fftSize = 2048;

      const source = context.createMediaStreamSource(stream);
      source.connect(analyser);

      if (!mountedRef.current || !enabledRef.current || startAttempt !== startAttemptRef.current) {
        source.disconnect();
        stream.getTracks().forEach((track) => track.stop());
        if (context.state !== "closed") {
          await context.close();
        }
        return;
      }

      streamRef.current = stream;
      audioContextRef.current = context;
      analyserRef.current = analyser;
      sourceRef.current = source;
      startedAtRef.current = Date.now();

      setIsRecording(true);
      setStatus("Listening…");
      drawWaveform();
    } catch (caughtError) {
      if (!mountedRef.current) return;
      const message = caughtError instanceof Error ? caughtError.message : "Unable to start recording.";
      setError(message);
      await stopRecording(false, "Recording failed");
    }
  };

  return (
    <div
      style={{
        display: "grid",
        gap: 12,
        padding: 14,
        borderRadius: 14,
        border: "1px solid rgba(148, 163, 184, 0.28)",
        background: "rgba(15, 23, 42, 0.94)",
        color: "#e2e8f0",
      }}
    >
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 12 }}>
        <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
          <button
            type="button"
            disabled={!enabled}
            onClick={() => {
              if (isRecording) {
                void stopRecording();
              } else {
                void startRecording();
              }
            }}
            style={{
              display: "inline-flex",
              alignItems: "center",
              gap: 8,
              padding: "10px 14px",
              borderRadius: 999,
              border: "1px solid rgba(248, 250, 252, 0.18)",
              background: !enabled ? "rgba(71, 85, 105, 0.5)" : isRecording ? "rgba(127, 29, 29, 0.95)" : "rgba(79, 70, 229, 0.95)",
              color: "#fff",
              cursor: enabled ? "pointer" : "not-allowed",
              fontWeight: 700,
            }}
          >
            <span
              ref={dotRef}
              aria-hidden="true"
              style={{
                width: 10,
                height: 10,
                borderRadius: "50%",
                background: isRecording ? "#ef4444" : "rgba(255, 255, 255, 0.8)",
                boxShadow: isRecording ? "0 0 0 6px rgba(239, 68, 68, 0.18)" : "none",
              }}
            />
            {isRecording ? "Stop recording" : "Start voice input"}
          </button>

          <div>
            <div style={{ fontWeight: 700 }}>Deepgram STT UI</div>
            <div style={{ fontSize: 12, color: "rgba(226, 232, 240, 0.76)" }}>
              Language: {language} · UI-only preview
            </div>
          </div>
        </div>

        <span
          style={{
            padding: "4px 8px",
            borderRadius: 999,
            fontSize: 12,
            background: isRecording ? "rgba(239, 68, 68, 0.16)" : "rgba(148, 163, 184, 0.12)",
            color: isRecording ? "#fca5a5" : "#cbd5e1",
          }}
        >
          {status}
        </span>
      </div>

      <canvas
        ref={canvasRef}
        aria-label="Live voice waveform"
        style={{
          width: "100%",
          height: 72,
          borderRadius: 12,
          border: "1px solid rgba(148, 163, 184, 0.18)",
          background: "rgba(2, 6, 23, 0.65)",
        }}
      />

      <div style={{ fontSize: 12, color: "rgba(226, 232, 240, 0.72)" }}>
        Recording captures microphone audio locally and returns a simulated transcript when stopped.
      </div>

      {error ? <div style={{ color: "#fca5a5", fontSize: 12 }}>{error}</div> : null}
    </div>
  );
}
