import { useState, useRef, useEffect } from "react";
import { useAuth } from "../auth";

interface Message {
  role: "human" | "ai";
  content: string;
}

export function ChatPage() {
  const { user, logout } = useAuth();
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages, sending]);

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
    
    setMessages((prev) => [...prev, { role: "human", content: text }]);
    setSending(true);

    try {
      const res = await fetch("/v1/chat", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ message: text }),
      });

      if (!res.ok) {
        const err = await res.text();
        setMessages((prev) => [
          ...prev,
          { role: "ai", content: `Error: ${err}` },
        ]);
        return;
      }

      const data = await res.json();
      setMessages((prev) => [
        ...prev,
        { role: "ai", content: data.response },
      ]);
    } catch (err) {
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

  const userInitial = user?.name ? user.name.charAt(0).toUpperCase() : "U";

  return (
    <div className="chat-page">
      <header className="chat-header">
        <h1>Sidekick</h1>
        <div className="header-right">
          <div className="user-profile">
            <div className="user-avatar">{userInitial}</div>
            <span className="user-name">{user?.name}</span>
          </div>
          <button onClick={logout} className="logout-btn">
            Log out
          </button>
        </div>
      </header>

      <div className="messages-container">
        <div className="messages">
          {messages.length === 0 && (
            <div className="empty-state">
              <div className="empty-state-icon">✨</div>
              <h3>Welcome to Sidekick</h3>
              <p>Your AI assistant with long-term memory. How can I assist you today?</p>
            </div>
          )}
          {messages.map((msg, i) => (
            <div key={i} className={`message-wrapper ${msg.role}`}>
              <div className={`message-avatar ${msg.role}`}>
                {msg.role === "human" ? userInitial : "AI"}
              </div>
              <div className={`message ${msg.role}`}>
                <div className="message-content">{msg.content}</div>
              </div>
            </div>
          ))}
          {sending && (
            <div className={`message-wrapper ai`}>
              <div className={`message-avatar ai`}>AI</div>
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
            disabled={sending}
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
