//! Everything the agent is told, assembled from the same files the app renders.
//!
//! The chat answers from context rather than from tools. That is a security
//! decision before it is a design one: an agent with nothing to reach for
//! cannot be talked into reaching for it, which matters when the prompt is
//! coming from a stranger over SSH. It is also simply more accurate — the
//! answers come from the sheets that are the source of record, not from a model
//! guessing at a repository it has grepped.

use crate::about::About;
use crate::taste::Sheet;

pub fn build(about: &About, taste: &Sheet, projects: &[skysheet::data::Project]) -> String {
    let mut s = String::new();

    s.push_str(
        "You are the resident agent inside Prince Patel's terminal portfolio, which \
         people reach over SSH. Answer questions about him and his work from the \
         notes below, and general questions on their own merits.\n\n\
         How to answer here:\n\
         - Plain prose. No markdown, no headings, no bullet lists, no code fences: \
         this is rendered as typeset paragraphs in a terminal and the syntax shows.\n\
         - Short. Two or three paragraphs at most, often one.\n\
         - If the notes do not cover something, say so plainly rather than inventing \
         it. Made-up detail about a real person's work is the one unrecoverable \
         failure here.\n\
         - Speak about Prince in the third person. You are not him.\n\
         - You have no tools and no file access, by design. Do not offer to run \
         anything or to read the repository.\n\n",
    );

    s.push_str("== WHO ==\n");
    s.push_str(&format!(
        "{} — {}, {}. github {}.\n{}\nCurrently: {}\n\n",
        about.name, about.role, about.where_, about.github, about.pitch, about.now
    ));

    s.push_str("== WHERE HE HAS BEEN ==\n");
    for p in termap::place::load() {
        s.push_str(&format!(
            "{} ({}), {}, {} — {}. {}\n",
            p.name, p.kind, p.where_, p.years, p.role, p.note
        ));
    }

    s.push_str("\n== WHAT HE HAS BUILT ==\n");
    for p in projects {
        s.push_str(&format!(
            "\n{} [{}] — {}\n  {}\n  tools: {}\n",
            p.name,
            p.year,
            p.tag,
            p.stats,
            p.tools.join(", ")
        ));
        for b in &p.beats {
            s.push_str(&format!("  · {}: {}\n", b.head, b.body));
        }
        if p.draft {
            s.push_str("  (this description was written from a summary, not the source)\n");
        }
    }

    s.push_str("\n== WHAT HE LIKES, AND WHY ==\n");
    s.push_str(&taste.open);
    s.push('\n');
    for e in taste.figures.iter().chain(&taste.works) {
        // Marked as a quotation so the agent repeats it as one rather than
        // paraphrasing it back as though it were Prince's own sentence.
        s.push_str(&format!("\n{} ({}) — quoted: \u{201c}{}\u{201d} {}\n", e.name, e.from, e.quote, e.body));
    }
    s.push_str("\nThe threads running through those:\n");
    for e in &taste.threads {
        s.push_str(&format!("- {}: {}\n", e.name, e.body));
    }

    s.push_str(
        "\n== THIS APP ==\n\
         The thing you are inside is one Rust binary over ratatui, no other \
         dependencies. Sections: a landing page; an experience map that flies \
         between five real places on the Van Wijk & Nuij optimal zoom/pan path, \
         rendered from a 1.6 GB PMTiles archive of India into braille subpixels; \
         a projects carousel with extruded 3D marks and animated engineering \
         diagrams; an infinite sheet of tool logos; the taste essay above; and \
         this conversation, which runs a local agent over the Agent Client \
         Protocol in plan mode.\n",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_context_carries_every_section_and_stays_a_sane_size() {
        let about = crate::about::load();
        let taste = crate::taste::load();
        let projects =
            skysheet::data::parse(include_str!("../../skills/data/projects.txt")).unwrap();
        let c = build(&about, &taste, &projects);

        for needle in ["Prince", "netjail", "Snufkin", "Kapadwanj", "plan mode"] {
            assert!(c.contains(needle), "context is missing {needle:?}");
        }
        // Every question pays for this preamble once. Worth watching: it grew
        // from the data files, and those grow.
        assert!(c.len() > 4_000, "suspiciously short: {}", c.len());
        assert!(c.len() < 40_000, "context has got out of hand: {}", c.len());
    }
}
