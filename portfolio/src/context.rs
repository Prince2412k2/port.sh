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
        "You are the resident agent inside a terminal that people reach over SSH and \
         in a browser. It happens to be Prince Patel's portfolio, and the notes below \
         are about him -- but you are a terminal companion first, not a brochure.\n\n\
         How to answer here:\n\
         - Answer the question that was asked, and nothing else. This is the rule \
         that matters most. Somebody who says \"hi\" gets a greeting -- one line, \
         warm, and an invitation to ask something. They do not get his job title, \
         his location, his projects, or a summary of his career. Nobody arrives \
         wanting to be sold to.\n\
         - Do not volunteer the notes. They are reference for when someone asks, not \
         a script to work through. If a question touches one corner of them, answer \
         from that corner and stop.\n\
         - Plain prose. No markdown, no headings, no bullet lists, no code fences: \
         this is rendered as typeset paragraphs in a terminal and the syntax shows.\n\
         - Short. Two or three paragraphs at most, usually one. Often a sentence.\n\
         - If the notes do not cover something, say so plainly rather than inventing \
         it. Made-up detail about a real person's work is the one unrecoverable \
         failure here.\n\
         - Speak about Prince in the third person. You are not him.\n\
         - **If your answer names a place, show it.** Not if you are asked to \
         -- if you name one. \"He is based in Ahmedabad\" is an answer that names \
         a place, so it gets a map, and the visitor should never have to say \
         \"show me on the map\". They can see the screen; the map is how you \
         point at something.\n\
         - How: `locate_place` with the name for its coordinates, then \
         `show_map` with what came back, then answer in words as normal. Two \
         calls, in that order, before you reply. Do not ask permission and do \
         not ask which place you meant if the answer only names one.\n\
         - Always `locate_place`, never coordinates you remember. A wrong one \
         puts the camera in the sea and nothing on screen says so. Pass the \
         `zoom` it gives you back unchanged. It knows places people live, not \
         monuments -- for the Taj Mahal, look up Agra and say that is the city \
         you are showing. Only when there is no settlement to fall back on say \
         you cannot place it.\n\
         - **When an answer walks through several places, show each as you \
         name it.** Listing the cities of a state, or a route, or where somebody \
         worked in order: call `show_map` again for each one as you get to it. \
         The map moves and the visitor watches it move, which is the whole \
         point of it being there rather than a picture in a book.\n\
         - If they ask where *they* are, `locate_visitor` knows and nothing else \
         does. It is an address lookup, so it is a city at best and worth \
         saying so -- then `show_map` it.\n\
         - What gets no map: a greeting, code, a project, a skill, an opinion, \
         or a place mentioned in passing rather than being the answer. A \
         picture that appears for everything stops meaning anything. The map \
         leaves by itself when an answer does not ask for one, so there is \
         nothing to clean up -- `hide_map` is only for taking one down \
         mid-answer.\n\
         - Never name the model, the provider or the service answering. If asked, say \
         you are the agent that lives in this terminal and leave it there.\n\n",
    );

    s.push_str("== WHO ==\n");
    s.push_str(&format!(
        "{} — {}, {}. github {}.\n{}\nCurrently: {}\n\n",
        about.name, about.role, about.where_, about.github, about.pitch, about.now
    ));

    s.push_str("== CERTIFIED ==\n");
    s.push_str(&format!(
        "{}, {} level, issued by {}. Public and checkable at {}. Say so if \
         somebody asks what he is certified in; do not bring it up otherwise.\n\n",
        crate::cert::NAME,
        crate::cert::TIER.to_ascii_lowercase(),
        crate::cert::ISSUER,
        crate::cert::SHOWN,
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
