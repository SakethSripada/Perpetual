import { useEffect, useState } from "react";
import { DOWNLOAD_URL } from "./content";

const navItems = [
  { href: "#continuity", label: "Continuity" },
  { href: "#extension", label: "Extension" },
];

const toolLogos = [
  { name: "Claude Code", logo: "/logos/claude.svg" },
  { name: "Codex", logo: "/logos/openai.svg" },
  { name: "VS Code", logo: "/logos/vscode.svg" },
  { name: "Ollama", logo: "/logos/ollama.svg" },
  { name: "GitHub", logo: "/logos/github.svg" },
  { name: "Docker", logo: "/logos/docker.svg" },
  { name: "MCP", logo: "/logos/mcp.svg" },
];

const continuityCards = [
  {
    eyebrow: "Auto resume",
    title: "Limits hit. The task picks up where it stopped.",
    body: "Perpetual preserves transcript, repo state, queued turns, branch, and approvals so a capped session can resume without manual copy-paste.",
  },
  {
    eyebrow: "Agent switching",
    title: "Claude Code to Codex, Codex to Claude Code.",
    body: "When one provider stalls, the extension can continue the same task with another coding agent while keeping the diff reviewable.",
  },
  {
    eyebrow: "Local fallback",
    title: "Cloud unavailable? Keep moving locally.",
    body: "Route continuity work through a local model fallback for triage, context gathering, or the next safe step until the main agent is available again.",
  },
];

const developerBullets = [
  "100% free VS Code extension",
  "No new subscription layer",
  "Works with your existing agent CLIs",
  "Local-first state and reviewable diffs",
];

const sessionLog = [
  { time: "08:41:12", actor: "claude code", note: "usage limit reached" },
  { time: "08:41:12", actor: "perpetual", note: "context sealed — transcript, repo state, 2 queued turns" },
  { time: "08:41:15", actor: "codex", note: "resumed on fix/parser-timeout" },
  { time: "08:41:58", actor: "codex", note: "diff ready for review" },
];

export function App() {
  const hasScrolled = useHasScrolled();
  useScrollReveal();

  return (
    <div className="site-shell">
      <Header hasScrolled={hasScrolled} />
      <main>
        <Hero />
        <LogoBand />
        <ContinuitySection />
        <ExtensionSection />
        <FinalCta />
      </main>
      <Footer />
    </div>
  );
}

function Header({ hasScrolled }: { hasScrolled: boolean }) {
  return (
    <header className={`topbar${hasScrolled ? " is-scrolled" : ""}`}>
      <div className="topbar-inner">
        <a className="brand" href="#top" aria-label="Perpetual home">
          <BrandMark />
          <span>Perpetual</span>
        </a>
        <nav className="nav-links" aria-label="Primary navigation">
          {navItems.map((item) => (
            <a key={item.href} href={item.href}>
              {item.label}
            </a>
          ))}
        </nav>
        <a className="nav-cta" href={DOWNLOAD_URL}>
          Download free
        </a>
      </div>
    </header>
  );
}

function BrandMark() {
  return (
    <svg className="brand-mark" viewBox="0 0 24 24" aria-hidden="true">
      <path
        d="M12 4.25v5.6M12 14.15v5.6M7.15 16.55l3.85-3.6M13 12.95l3.85 3.6M7.15 7.45 11 11.05M13 11.05l3.85-3.6"
        fill="none"
        stroke="currentColor"
        strokeLinecap="round"
        strokeWidth="1.8"
      />
      <circle cx="12" cy="12" r="2.35" fill="currentColor" />
      <circle cx="12" cy="3.5" r="2.2" fill="currentColor" />
      <circle cx="5.75" cy="17.75" r="2.2" fill="currentColor" />
      <circle cx="18.25" cy="17.75" r="2.2" fill="currentColor" />
    </svg>
  );
}

function ArrowIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <path d="M5.25 3.75h7v7" fill="none" stroke="currentColor" strokeLinecap="round" strokeWidth="1.5" />
      <path d="m12.05 3.95-8.1 8.1" fill="none" stroke="currentColor" strokeLinecap="round" strokeWidth="1.5" />
    </svg>
  );
}

function Hero() {
  return (
    <section id="top" className="hero">
      <div className="hero-aurora" aria-hidden="true" />
      <div className="hero-inner">
        <h1>
          Make Claude Code<br className="hero-break" /> and Codex work together.
        </h1>
        <p className="hero-lede">
          Perpetual switches agents automatically, resumes work as soon as your limits are reset, and falls back to cloud runs to ensure your work is never interrupted.
        </p>
        <div className="cta-row">
          <a className="button primary" href={DOWNLOAD_URL}>
            <span>Install for free</span>
            <ArrowIcon />
          </a>
        </div>
        <p className="availability">
          No new subscription. Works with your existing Claude Code and Codex plans.
        </p>
        <SessionCard />
      </div>
    </section>
  );
}

function SessionCard() {
  return (
    <div className="handoff-card" aria-label="Example agent handoff session">
      <div className="handoff-log">
        {sessionLog.map((row) => (
          <div className="log-row" key={`${row.time}-${row.note}`} data-actor={row.actor}>
            <span className="log-time">{row.time}</span>
            <span className="log-actor">{row.actor}</span>
            <span className="log-note">{row.note}</span>
          </div>
        ))}
      </div>
      <div className="handoff-agents">
        <AgentPill logo="/logos/claude.svg" name="Claude Code" state="limit hit" />
        <AgentPill logo="/logos/openai.svg" name="Codex" state="active" tone="active" />
        <AgentPill logo="/logos/ollama.svg" name="Local model" state="standby" />
      </div>
    </div>
  );
}

function AgentPill({
  logo,
  name,
  state,
  tone,
}: {
  logo: string;
  name: string;
  state: string;
  tone?: "active";
}) {
  return (
    <div className="agent-pill" data-tone={tone}>
      <img src={logo} alt="" decoding="async" />
      <div>
        <strong>{name}</strong>
        <span>{state}</span>
      </div>
    </div>
  );
}

function LogoBand() {
  return (
    <section className="logo-band" aria-label="Supported tools">
      <span className="logo-copy">Works with the tools you already use</span>
      <div className="marquee">
        <div className="marquee-track">
          <div className="logo-strip">
            {toolLogos.map((tool) => (
              <span key={tool.name}>
                <img className="logo-mark" src={tool.logo} alt="" loading="lazy" decoding="async" />
                {tool.name}
              </span>
            ))}
          </div>
          <div className="logo-strip" aria-hidden="true">
            {toolLogos.map((tool) => (
              <span key={`${tool.name}-dup`}>
                <img className="logo-mark" src={tool.logo} alt="" loading="lazy" decoding="async" />
                {tool.name}
              </span>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}

function ContinuitySection() {
  return (
    <section id="continuity" className="section">
      <div className="section-head" data-reveal>
        <p className="kicker">Continuity</p>
        <h2>Stop losing work when the best agent for the moment taps out.</h2>
      </div>
      <div className="feature-grid">
        {continuityCards.map((card) => (
          <article className="feature-card" key={card.title} data-reveal>
            <p>{card.eyebrow}</p>
            <h3>{card.title}</h3>
            <span>{card.body}</span>
          </article>
        ))}
      </div>
    </section>
  );
}

function ExtensionSection() {
  return (
    <section id="extension" className="section">
      <div className="section-head" data-reveal>
        <p className="kicker">The extension</p>
        <h2>Install it, point it at your existing agents, and keep shipping.</h2>
      </div>
      <div className="extension-panel" data-reveal>
        <div className="extension-copy">
          <p>
            Perpetual is built for developers who want more reliable agentic coding without
            adding another paid product. Your provider auth stays with the existing CLIs, while the
            extension handles continuity and review.
          </p>
          <ul>
            {developerBullets.map((bullet) => (
              <li key={bullet}>{bullet}</li>
            ))}
          </ul>
        </div>
        <div className="extension-support" aria-label="Supported agents">
          <p className="extension-support-label">Works with your existing CLIs</p>
          <ul className="support-agents">
            <li className="support-agent">
              <img src="/logos/claude.svg" alt="" decoding="async" />
              <span>Claude Code</span>
            </li>
            <li className="support-agent">
              <img src="/logos/openai.svg" alt="" decoding="async" />
              <span>Codex</span>
            </li>
            <li className="support-agent">
              <img src="/logos/ollama.svg" alt="" decoding="async" />
              <span>Local fallback</span>
            </li>
          </ul>
        </div>
      </div>
    </section>
  );
}

function FinalCta() {
  return (
    <section className="final-cta" data-reveal>
      <div className="final-aurora" aria-hidden="true" />
      <h2>The best way to build 24/7</h2>
      <div className="cta-row">
        <a className="button primary" href={DOWNLOAD_URL}>
          <span>Download free extension</span>
          <ArrowIcon />
        </a>
      </div>
    </section>
  );
}

function Footer() {
  return (
    <footer className="footer">
      <span>Perpetual</span>
      <span>Free VS Code extension for agent continuity.</span>
    </footer>
  );
}

function useHasScrolled() {
  const [hasScrolled, setHasScrolled] = useState(false);

  useEffect(() => {
    let frame = 0;

    const update = () => {
      setHasScrolled((current) => {
        const next = window.scrollY > 24;
        return current === next ? current : next;
      });
    };

    const schedule = () => {
      if (frame) return;
      frame = window.requestAnimationFrame(() => {
        frame = 0;
        update();
      });
    };

    update();
    window.addEventListener("scroll", schedule, { passive: true });
    window.addEventListener("resize", schedule);

    return () => {
      if (frame) window.cancelAnimationFrame(frame);
      window.removeEventListener("scroll", schedule);
      window.removeEventListener("resize", schedule);
    };
  }, []);

  return hasScrolled;
}

function useScrollReveal() {
  useEffect(() => {
    const targets = Array.from(document.querySelectorAll<HTMLElement>("[data-reveal]"));

    if (!("IntersectionObserver" in window)) {
      targets.forEach((target) => target.classList.add("is-visible"));
      return;
    }

    const observer = new IntersectionObserver(
      (entries) => {
        entries.forEach((entry) => {
          if (!entry.isIntersecting) return;
          entry.target.classList.add("is-visible");
          observer.unobserve(entry.target);
        });
      },
      { rootMargin: "0px 0px -10% 0px", threshold: 0.1 }
    );

    targets.forEach((target) => observer.observe(target));
    return () => observer.disconnect();
  }, []);
}
