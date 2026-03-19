import { useState, useRef, useEffect, useCallback } from "react";
import Markdown from "react-markdown";
import { useAuth } from "../auth";

interface Message {
  id?: number;
  role: "human" | "ai";
  content: string;
  timestamp?: string;
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
  const [loadingHistory, setLoadingHistory] = useState(false);
  const [hasMore, setHasMore] = useState(true);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const messagesTopRef = useRef<HTMLDivElement>(null);
  const messagesContainerRef = useRef<HTMLDivElement>(null);
  const headerRef = useRef<HTMLElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const initialLoadDone = useRef(false);

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

      // Preserve scroll position when prepending older messages.
      if (before !== undefined && container) {
        requestAnimationFrame(() => {
          container.scrollTop = container.scrollHeight - prevScrollHeight;
        });
      }
    } finally {
      setLoadingHistory(false);
    }
  }, [loadingHistory]);

  // Load initial history on mount.
  useEffect(() => {
    if (initialLoadDone.current) return;
    initialLoadDone.current = true;
    loadHistory().then(() => {
      // Scroll to bottom after initial load and sync scroll tracking.
      requestAnimationFrame(() => {
        messagesEndRef.current?.scrollIntoView();
      });
    });
  }, [loadHistory]);

  // Auto-scroll to bottom when the user sends or receives a message.
  const prevMessageCount = useRef(0);
  useEffect(() => {
    // Only auto-scroll when messages are appended (new), not prepended (history).
    if (messages.length > prevMessageCount.current && prevMessageCount.current > 0) {
      messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
    }
    prevMessageCount.current = messages.length;
  }, [messages.length]);

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

  const sendMessage = async () => {
    const text = input.trim();
    if (!text || sending) return;

    setInput("");
    if (textareaRef.current) {
      textareaRef.current.style.height = "auto";
    }

    setMessages((prev) => [...prev, { role: "human", content: text, timestamp: new Date().toISOString() }]);
    setSending(true);

    try {
      const res = await fetch("/v1/chat", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ message: text, local_time: new Date().toString() }),
      });

      if (!res.ok) {
        const err = await res.text();
        console.error(`Chat API error (${res.status}):`, err);
        setMessages((prev) => [
          ...prev,
          { role: "ai", content: `Error: ${err}` },
        ]);
        return;
      }

      const data = await res.json();
      setMessages((prev) => [
        ...prev,
        { role: "ai", content: data.response, timestamp: new Date().toISOString() },
      ]);
    } catch (err) {
      console.error("Chat network error:", err);
      setMessages((prev) => [
        ...prev,
        { role: "ai", content: `Network error: ${err}` },
      ]);
    } finally {
      setSending(false);
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
            <div key={msg.id ?? `pending-${i}`} className={`message-wrapper ${msg.role}`}>
              <div className={`message-avatar ${msg.role}`}>
                {msg.role === "human" ? (user?.picture ? <img src={user.picture} alt={user.name} referrerPolicy="no-referrer" /> : (user?.name ? user.name.charAt(0).toUpperCase() : "U")) : <img src="/icons/icon-192.png" alt="Sidekick" />}
              </div>
              <div className={`message ${msg.role}`}>
                <div className="message-content">
                  {msg.role === "ai" ? <Markdown>{msg.content}</Markdown> : msg.content}
                </div>
                {msg.timestamp && <div className="message-time">{formatTimestamp(msg.timestamp)}</div>}
              </div>
            </div>
          ))}
          {sending && (
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
