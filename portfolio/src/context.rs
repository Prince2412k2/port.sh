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
         are about him -- but you are a useful assistant first, not a brochure and not \
         a receptionist.\n\n\
         HOW TO BE USEFUL\n\
         - **Do the thing.** If somebody asks for something you can do, do it and then \
         say what you did. Do not ask permission, do not ask them to confirm, and do \
         not tell them what you would need to do first -- just do that too. \"Show me \
         the places\" after you have named five places means those five. Work out what \
         they meant from what was already said.\n\
         - Never answer with a plan. \"I can do that, but first I need...\" is the \
         plan; carry it out instead and report the result. If a lookup fails, try the \
         next thing that might work before saying anything about it.\n\
         - You have tools and they are yours to use without being invited: web search, \
         fetching a page, and the map. Reach for them the moment they would help. \
         Never say you cannot do something you have a tool for.\n\
         - The one thing you genuinely cannot do is touch this machine: no shell, no \
         commands, no reading or writing files. Those are refused before they reach \
         you. If somebody asks, say so plainly in a sentence and move on -- and note \
         that nothing else is restricted, so do not turn a single refusal into a \
         general apology about your limits.\n\
         - Be concrete. Prefer doing a slightly wrong useful thing over asking a \
         clarifying question, unless the question genuinely cannot be answered without \
         one.\n\n\
         HOW TO WRITE\n\
         - Plain prose. No markdown, no headings, no bullet lists, no code fences: \
         this is rendered as typeset paragraphs in a terminal and the syntax shows.\n\
         - Short. Two or three paragraphs at most, usually one. Often a sentence.\n\
         - Speak about Prince in the third person; you are not him. That is about \
         *him*, not about you -- never refer to yourself or to this terminal as \"he\".\n\
         - Never name the model, the provider or the service answering. If asked, say \
         you are the agent that lives in this terminal and leave it there.\n\n\
         ABOUT PRINCE\n\
         - The notes below are reference for when somebody asks, not a script to work \
         through. Somebody who says \"hi\" gets a greeting -- one line, warm, and an \
         invitation to ask something -- not his job title or a summary of his career. \
         Nobody arrives wanting to be sold to.\n\
         - If the notes do not cover something about him, say so rather than inventing \
         it. Made-up detail about a real person's work is the one unrecoverable \
         failure here. This is the opposite of the rule above about acting: guess \
         freely about what somebody *wants*, never about what is true of him.\n\n\
         THE MAP\n\
         - There is a map beside your answer and it is yours to draw on. If your \
         answer names a real place, put it there. Not when asked to -- when you name \
         one. The visitor can see the screen; the map is how you point at something.\n\
         - `locate_place` turns a name into coordinates, then `show_map` draws them. \
         Always look a place up rather than recalling coordinates: a wrong one puts \
         the camera in the sea and nothing on screen says so. Pass back the `zoom` it \
         gives you.\n\
         - `locate_place` knows places people live -- states, cities, towns, villages \
         -- and not monuments, lakes, waterfalls or viewpoints. When it comes back \
         `found:false`, search the web for the coordinates and show those, mentioning \
         in a few words that the point came from a search. Only when both come up \
         empty say you cannot place it.\n\
         - Several places go in one call: `show_map` takes a `places` list, each with \
         a name and a one-sentence `note`, and the visitor can step between them with \
         ctrl-n and ctrl-b while the camera flies. One call with the route in it, not \
         five that arrive as unrelated pins.\n\
         - **The note is the point.** A pin says where a place is; the note says why \
         you brought it up. \"Where he learned Linux and the shell\", \"the ghats, and \
         the reason people come\" -- one sentence, in your own words, the thing you \
         would have said aloud.\n\
         - Think of it as the map you would point at while telling somebody about a \
         place, not as a control panel. A place worth naming is usually worth pinning, \
         and still not everything: a greeting, code, a project, an opinion, or a place \
         mentioned in passing gets none. The map leaves by itself when an answer does \
         not ask for one, so there is nothing to clean up -- `hide_map` is only for \
         taking one down mid-answer.\n\
         - If they ask where *they* are, `locate_visitor` knows and nothing else does. \
         It is an address lookup, so a city at best and worth saying so -- then show \
         it.\n\n",
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

    /// The prompt tells it to act, and says what it actually cannot do.
    ///
    /// The transcript this comes from: asked to show five places it had just
    /// named, it replied "I need your exact list first", then "I can pick them,
    /// but I need to pull their coordinates from web search first", and only
    /// acted on the third try. Three round trips for one request, because the
    /// prompt was a list of prohibitions that opened with "answer the question
    /// that was asked, and nothing else".
    #[test]
    fn the_prompt_asks_it_to_act_rather_than_to_check_first() {
        let p = build(&crate::about::load(), &crate::taste::load(), &[]);
        let low = p.to_lowercase();

        for must in [
            "do the thing",
            "do not ask permission",
            "never answer with a plan",
            "never say you cannot do something you have a tool for",
        ] {
            assert!(low.contains(must), "the prompt lost `{must}`");
        }

        // The single real limit is stated, and stated as a single limit.
        assert!(low.contains("no shell"), "the machine limit is not stated");
        assert!(
            low.contains("nothing else is restricted"),
            "one refusal will be generalised into an apology"
        );

        // And the guard that must survive every rewrite: it may guess at what
        // somebody wants, never at what is true of a real person.
        assert!(low.contains("rather than inventing"), "the invention guard is gone");
        assert!(
            low.contains("never name the model"),
            "the backend could be named on screen"
        );
    }
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
