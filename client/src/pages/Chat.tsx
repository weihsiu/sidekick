import { useState, useRef, useEffect, useLayoutEffect, useCallback } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { useAuth } from "../auth";

interface Message {
  id?: number;
  role: "human" | "ai";
  content: string;
  timestamp?: string;
  pending?: boolean;
  clientKey?: string;
}

function formatTimestamp(ts: string): string {
  const date = new Date(ts);
  const now = new Date();
  const isToday =
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate();
  const isThisYear = date.getFullYear() === now.getFullYear();

  if (isToday) {
    return date.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
  } else if (isThisYear) {
    return date.toLocaleString(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
  } else {
    return date.toLocaleString(undefined, { year: "numeric", month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
  }
}

const PAGE_SIZE = 20;

export function ChatPage() {
  const { user, logout } = useAuth();
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [recording, setRecording] = useState(false);
  const [transcribing, setTranscribing] = useState(false);
  // true while the AI is processing any message (shown on all connected clients)
  const [typing, setTyping] = useState(false);
  const [loadingHistory, setLoadingHistory] = useState(false);
  const [hasMore, setHasMore] = useState(true);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const messagesTopRef = useRef<HTMLDivElement>(null);
  const messagesContainerRef = useRef<HTMLDivElement>(null);
  const headerRef = useRef<HTMLElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const mediaRecorderRef = useRef<MediaRecorder | null>(null);
  const audioContextRef = useRef<AudioContext | null>(null);
  const silenceTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const initialLoadDone = useRef(false);
  const sseConnected = useRef(false);
  const lastMessageIdRef = useRef<number | undefined>(undefined);

  const loadHistory = useCallback(async (before?: number) => {
    if (loadingHistory) return;
    setLoadingHistory(true);

    try {
      const params = new URLSearchParams({ limit: String(PAGE_SIZE), category: "conversation" });
      if (before !== undefined) params.set("before", String(before));

      const res = await fetch(`/v1/history?${params}`);
      if (!res.ok) return;

      const older: Message[] = await res.json();
      if (older.length < PAGE_SIZE) setHasMore(false);
      if (older.length === 0) return;

      const container = messagesContainerRef.current;
      const prevScrollHeight = container?.scrollHeight ?? 0;

      setMessages((prev) => [...older, ...prev]);

      if (before !== undefined && container) {
        requestAnimationFrame(() => {
          container.scrollTop = container.scrollHeight - prevScrollHeight;
        });
      }
    } finally {
      setLoadingHistory(false);
    }
  }, [loadingHistory]);

  // Keep lastMessageIdRef current so the SSE onopen handler can read it
  // without stale closure issues.
  useEffect(() => {
    const last = [...messages].reverse().find((m) => m.id !== undefined);
    if (last?.id !== undefined) lastMessageIdRef.current = last.id;
  }, [messages]);

  // SSE connection — receives human messages and AI responses for this user
  // across all connected clients.
  useEffect(() => {
    if (!user) return;
    sseConnected.current = false;
    const es = new EventSource("/v1/events");

    es.onopen = () => {
      if (!sseConnected.current) {
        sseConnected.current = true;
        return; // initial connect — history already loaded by loadHistory useEffect
      }
      // Reconnect: fetch any messages that arrived while offline and append them.
      const after = lastMessageIdRef.current;
      if (after === undefined) return;
      fetch(`/v1/history?after=${after}&category=conversation`)
        .then((r) => (r.ok ? r.json() : []))
        .then((missed: Message[]) => {
          if (missed.length > 0) setMessages((prev) => {
            const existingIds = new Set(prev.map((m) => m.id).filter((id) => id !== undefined));
            const newMessages = missed.filter((m) => m.id === undefined || !existingIds.has(m.id));
            return newMessages.length > 0 ? [...prev, ...newMessages] : prev;
          });
        })
        .catch((err) => console.error("Failed to fetch missed messages on reconnect:", err));
    };

    es.onmessage = (e) => {
      const event = JSON.parse(e.data) as {
        type: "human_message" | "ai_response" | "error";
        id?: number;
        content?: string;
        message?: string;
        timestamp?: string;
      };
      if (event.type === "human_message") {
        if (event.id !== undefined) lastMessageIdRef.current = event.id;
        setMessages((prev) => {
          // Replace the optimistic pending message if one exists, otherwise append.
          const idx = prev.findLastIndex((m: Message) => m.role === "human" && m.pending);
          if (idx !== -1) {
            const next = [...prev];
            // Preserve clientKey so the React key stays stable and avoids a flash.
            next[idx] = { id: event.id, role: "human", content: event.content!, timestamp: event.timestamp, clientKey: prev[idx].clientKey };
            return next;
          }
          return [...prev, { id: event.id, role: "human", content: event.content!, timestamp: event.timestamp }];
        });
        setTyping(true);
      } else if (event.type === "ai_response") {
        if (event.id !== undefined) lastMessageIdRef.current = event.id;
        setMessages((prev) => [
          ...prev,
          { id: event.id, role: "ai", content: event.content!, timestamp: event.timestamp },
        ]);
        setSending(false);
        setTyping(false);
      } else if (event.type === "error") {
        setMessages((prev) => [...prev, { role: "ai", content: event.message ?? "Something went wrong." }]);
        setSending(false);
        setTyping(false);
      }
    };

    return () => es.close();
  }, [user]);

  // Clean up recording on unmount.
  useEffect(() => {
    return () => {
      if (silenceTimerRef.current) clearInterval(silenceTimerRef.current);
      audioContextRef.current?.close();
      if (mediaRecorderRef.current?.state !== "inactive") mediaRecorderRef.current?.stop();
    };
  }, []);

  // Load initial history on mount.
  useEffect(() => {
    if (initialLoadDone.current) return;
    initialLoadDone.current = true;
    loadHistory();
  }, [loadHistory]);

  // Set initial scroll position to the bottom synchronously before the first paint,
  // so the user never sees the list start from the top.
  const initialScrollDone = useRef(false);
  useLayoutEffect(() => {
    if (initialScrollDone.current || loadingHistory || messages.length === 0) return;
    const container = messagesContainerRef.current;
    if (!container) return;
    container.style.scrollBehavior = "auto";
    container.scrollTop = container.scrollHeight;
    container.style.scrollBehavior = "";
    initialScrollDone.current = true;
  }, [messages, loadingHistory]);

  // Auto-scroll to bottom when the user sends or receives a message.
  const prevLastKeyRef = useRef<string | undefined>(undefined);
  useEffect(() => {
    if (messages.length === 0) return;
    const last = messages[messages.length - 1];
    const lastKey = last.clientKey ?? (last.id !== undefined ? String(last.id) : undefined);
    // Only scroll when the last message changed — i.e., something was appended.
    // Prepending history changes messages.length but leaves the last message the same.
    if (lastKey !== undefined && lastKey !== prevLastKeyRef.current && prevLastKeyRef.current !== undefined) {
      messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
    }
    prevLastKeyRef.current = lastKey;
  }, [messages]);

  // Smart header: show when scrolling up, hide when scrolling down.
  const lastScrollTop = useRef(0);
  useEffect(() => {
    const container = messagesContainerRef.current;
    const header = headerRef.current;
    if (!container || !header) return;

    const onScroll = () => {
      const st = container.scrollTop;
      if (st > lastScrollTop.current + 5) {
        header.classList.add("chat-header-hidden");
      } else if (st < lastScrollTop.current - 5) {
        header.classList.remove("chat-header-hidden");
      }
      lastScrollTop.current = st;
    };

    container.addEventListener("scroll", onScroll, { passive: true });
    return () => container.removeEventListener("scroll", onScroll);
  }, []);

  // Infinite scroll — load older messages when scrolling to top.
  const handleScroll = useCallback(() => {
    const container = messagesContainerRef.current;
    if (!container || !hasMore || loadingHistory) return;

    if (container.scrollTop < 100) {
      const oldest = messages.find((m) => m.id !== undefined);
      if (oldest?.id) loadHistory(oldest.id);
    }
  }, [hasMore, loadingHistory, messages, loadHistory]);

  const handleInput = (e: React.ChangeEvent<HTMLTextAreaElement>) => {
    setInput(e.target.value);
    e.target.style.height = "auto";
    e.target.style.height = `${Math.min(e.target.scrollHeight, 150)}px`;
  };

  const sendMessageText = async (text: string) => {
    setSending(true);
    const handleError = (msg: string) => {
      setMessages((prev) => [...prev, { role: "ai", content: msg }]);
      setSending(false);
      setTyping(false);
    };
    try {
      const res = await fetch("/v1/chat", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ message: text, local_time: new Date().toString() }),
      });
      if (!res.ok) {
        const err = await res.text();
        console.error(`Chat API error (${res.status}):`, err);
        handleError(`Error: ${err}`);
        return;
      }
      // On success (200 or 202), human message and AI response arrive via SSE.
    } catch (err) {
      console.error("Chat network error:", err);
      handleError(`Network error: ${err}`);
    }
  };

  const sendMessage = async () => {
    const text = input.trim();
    if (!text || sending) return;
    setInput("");
    if (textareaRef.current) textareaRef.current.style.height = "auto";
    // Optimistically show the human message immediately with a stable key.
    setMessages((prev) => [...prev, { role: "human", content: text, pending: true, clientKey: crypto.randomUUID() }]);
    await sendMessageText(text);
  };

  const transcribeAndSend = async (blob: Blob, mimeType: string) => {
    setTranscribing(true);
    try {
      const localTime = encodeURIComponent(new Date().toString());
      const res = await fetch(`/v1/voice?local_time=${localTime}`, {
        method: "POST",
        headers: { "Content-Type": mimeType },
        body: blob,
      });
      if (!res.ok && res.status !== 204) throw new Error(await res.text());
      if (res.status === 202) setSending(true); // show typing indicator while LLM runs
    } catch (err) {
      console.error("Voice error:", err);
      setMessages((prev) => [...prev, { role: "ai", content: `Voice error: ${err}` }]);
    } finally {
      setTranscribing(false);
    }
  };

  const stopRecording = () => {
    if (silenceTimerRef.current) {
      clearInterval(silenceTimerRef.current);
      silenceTimerRef.current = null;
    }
    audioContextRef.current?.close();
    audioContextRef.current = null;
    mediaRecorderRef.current?.stop();
    mediaRecorderRef.current = null;
    setRecording(false);
  };

  const toggleRecording = async () => {
    if (recording) {
      stopRecording();
      return;
    }
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const mimeType = MediaRecorder.isTypeSupported("audio/webm;codecs=opus")
        ? "audio/webm;codecs=opus"
        : MediaRecorder.isTypeSupported("audio/mp4")
        ? "audio/mp4"
        : "audio/ogg;codecs=opus";
      const mr = new MediaRecorder(stream, { mimeType });
      const chunks: Blob[] = [];
      mr.ondataavailable = (e) => { if (e.data.size > 0) chunks.push(e.data); };
      mr.onstop = () => {
        stream.getTracks().forEach((t) => t.stop());
        const baseMime = mimeType.split(";")[0];
        transcribeAndSend(new Blob(chunks, { type: baseMime }), baseMime);
      };
      mr.start();
      mediaRecorderRef.current = mr;
      setRecording(true);

      // Silence detection: stop automatically after 1.5s of quiet.
      const ctx = new AudioContext();
      const analyser = ctx.createAnalyser();
      analyser.fftSize = 256;
      ctx.createMediaStreamSource(stream).connect(analyser);
      audioContextRef.current = ctx;

      const freqData = new Uint8Array(analyser.frequencyBinCount);
      const SILENCE_THRESHOLD = 10;   // 0–255 frequency bin average
      const SILENCE_DURATION_MS = 1000;
      const MIN_RECORDING_MS = 500;   // don't stop before speech has a chance to start
      const recordingStart = Date.now();
      let silenceStart: number | null = null;

      silenceTimerRef.current = setInterval(() => {
        analyser.getByteFrequencyData(freqData);
        const avg = freqData.reduce((a, b) => a + b, 0) / freqData.length;

        if (Date.now() - recordingStart < MIN_RECORDING_MS) return;

        if (avg < SILENCE_THRESHOLD) {
          if (!silenceStart) silenceStart = Date.now();
          else if (Date.now() - silenceStart >= SILENCE_DURATION_MS) stopRecording();
        } else {
          silenceStart = null;
        }
      }, 100);
    } catch (err) {
      console.error("Microphone access error:", err);
      setRecording(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      sendMessage();
    }
  };

  const userAvatar = user?.picture ? (
    <img className="user-avatar" src={user.picture} alt={user.name} referrerPolicy="no-referrer" />
  ) : (
    <div className="user-avatar">{user?.name ? user.name.charAt(0).toUpperCase() : "U"}</div>
  );

  return (
    <div className="chat-page">
      <header ref={headerRef} className="chat-header">
        <h1>Sidekick</h1>
        <div className="header-right">
          <div className="user-profile">
            {userAvatar}
            <span className="user-name">{user?.name}</span>
          </div>
          <button onClick={logout} className="logout-btn">
            Log out
          </button>
        </div>
      </header>

      <div className="messages-container" ref={messagesContainerRef} onScroll={handleScroll}>
        <div className="messages">
          {loadingHistory && (
            <div className="history-loading">Loading older messages...</div>
          )}
          {!hasMore && messages.length > 0 && (
            <div className="history-end">Beginning of conversation</div>
          )}
          <div ref={messagesTopRef} />
          {messages.length === 0 && !loadingHistory && (
            <div className="empty-state">
              <div className="empty-state-icon">✨</div>
              <h3>Welcome to Sidekick</h3>
              <p>Your AI assistant with long-term memory. How can I assist you today?</p>
            </div>
          )}
          {messages.map((msg, i) => (
            <div key={msg.clientKey ?? msg.id ?? `idx-${i}`} className={`message-wrapper ${msg.role}`}>
              <div className={`message-avatar ${msg.role}`}>
                {msg.role === "human" ? (user?.picture ? <img src={user.picture} alt={user.name} referrerPolicy="no-referrer" /> : (user?.name ? user.name.charAt(0).toUpperCase() : "U")) : <img src="/icons/icon-192.png" alt="Sidekick" />}
              </div>
              <div className={`message ${msg.role}`}>
                <div className="message-content">
                  {msg.role === "ai" ? (
                    <Markdown remarkPlugins={[remarkGfm]} components={{ a: ({ href, children }) => <a href={href} target="_blank" rel="noopener noreferrer">{children}</a> }}>{msg.content}</Markdown>
                  ) : (
                    (() => {
                      const parts = msg.content.split(/(https?:\/\/[^\s]+)/g);
                      return parts.map((part, j) =>
                        part.startsWith("http://") || part.startsWith("https://") ? <a key={j} href={part} target="_blank" rel="noopener noreferrer">{part}</a> : part
                      );
                    })()
                  )}
                </div>
                {msg.timestamp && <div className="message-time">{formatTimestamp(msg.timestamp)}</div>}
              </div>
            </div>
          ))}
          {(sending || typing) && (
            <div className={`message-wrapper ai`}>
              <div className={`message-avatar ai`}><img src="/icons/icon-192.png" alt="Sidekick" /></div>
              <div className={`message ai`}>
                <div className="message-content">
                  <div className="typing-indicator">
                    <span />
                    <span />
                    <span />
                  </div>
                </div>
              </div>
            </div>
          )}
          <div ref={messagesEndRef} />
        </div>
      </div>

      <div className="input-container">
        <div className="input-area">
          <textarea
            ref={textareaRef}
            value={input}
            onChange={handleInput}
            onKeyDown={handleKeyDown}
            placeholder="Type your message..."
            rows={1}
            autoFocus
          />
          {typeof window !== "undefined" && "MediaRecorder" in window && (
            <button
              className={`mic-btn${recording ? " recording" : ""}${transcribing ? " transcribing" : ""}`}
              onClick={toggleRecording}
              disabled={sending || transcribing}
              title={recording ? "Stop recording" : "Voice input"}
            >
              {recording ? (
                <svg viewBox="0 0 24 24" width="20" height="20" fill="currentColor">
                  <rect x="6" y="6" width="12" height="12" rx="2" />
                </svg>
              ) : transcribing ? (
                <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <circle cx="12" cy="12" r="9" strokeDasharray="28 56" strokeDashoffset="0">
                    <animateTransform attributeName="transform" type="rotate" from="0 12 12" to="360 12 12" dur="1s" repeatCount="indefinite" />
                  </circle>
                </svg>
              ) : (
                <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <rect x="9" y="2" width="6" height="13" rx="3" />
                  <path d="M5 10a7 7 0 0 0 14 0" />
                  <line x1="12" y1="19" x2="12" y2="22" />
                  <line x1="9" y1="22" x2="15" y2="22" />
                </svg>
              )}
            </button>
          )}
          <button className="send-btn" onClick={sendMessage} disabled={sending || !input.trim()}>
            <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <line x1="22" y1="2" x2="11" y2="13" />
              <polygon points="22 2 15 22 11 13 2 9 22 2" />
            </svg>
          </button>
        </div>
      </div>
    </div>
  );
}
