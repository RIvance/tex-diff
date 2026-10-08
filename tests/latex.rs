use std::{fs, path::Path, time::Duration};
use tex_diff::latex::{self, Input, Kind, Review};

const PREAMBLE: &str = "\\documentclass{article}\n";

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new(old: &str, new: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for side in ["old", "new"] {
            fs::create_dir(dir.path().join(side)).unwrap();
        }
        let f = Self { dir };
        f.document("old", "", old);
        f.document("new", "", new);
        f
    }

    fn write(&self, side: &str, name: &str, content: &str) {
        let path = self.dir.path().join(side).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn document(&self, side: &str, declarations: &str, body: &str) {
        self.write(
            side,
            "main.tex",
            &format!("{PREAMBLE}{declarations}\\begin{{document}}{body}\\end{{document}}"),
        );
    }

    fn compare(&self) -> anyhow::Result<Review> {
        let old = self.dir.path().join("old");
        let new = self.dir.path().join("new");
        latex::compare(
            Some(Input {
                root: &old,
                main: Path::new("main.tex"),
            }),
            Some(Input {
                root: &new,
                main: Path::new("main.tex"),
            }),
            &self.dir.path().join("output"),
            "index",
            "working tree",
            Duration::from_secs(5),
        )
    }
}

fn texts(review: &Review) -> Vec<&str> {
    review
        .report
        .changes
        .iter()
        .map(|c| c.text.as_str())
        .collect()
}

#[test]
fn changing_one_word_marks_its_whole_sentence_and_preserves_everything_else() {
    let f = Fixture::new(
        "An old sentence. This remains.\\section{Appendix}All appendix contents stay.",
        "An new sentence. This remains.\\section{Appendix}All appendix contents stay.",
    );
    let review = f.compare().unwrap();
    assert_eq!(texts(&review), ["An old sentence.", "An new sentence."]);
    let tex = fs::read_to_string(review.source).unwrap();
    assert!(tex.contains("This remains.") && tex.contains("All appendix contents stay."));
    assert!(tex.contains("TexDiffRemovedColor") && tex.contains("TexDiffAddedColor"));
    assert!(tex.contains("An old sentence.") && tex.contains("An new sentence."));
}

#[test]
fn whitespace_comments_and_nested_input_refactors_are_ignored() {
    let f = Fixture::new("One sentence. The next stays.", "\\include{parts/body}");
    f.write("new", "parts/body.tex", "% invisible\n\\input{parts/text}");
    f.write(
        "new",
        "parts/text.tex",
        "One\n sentence.\nThe   next stays.\n",
    );
    assert!(!f.compare().unwrap().report.changed);
}

#[test]
fn flattened_inputs_do_not_turn_end_of_line_spaces_into_paragraph_breaks() {
    let body = "\\input{diagram}\n\\[x=1\\]Following text.";
    let f = Fixture::new(body, body);
    for side in ["old", "new"] {
        f.write(
            side,
            "diagram.tex",
            "\\begin{figure}Diagram contents.\\end{figure}\n",
        );
    }
    let review = f.compare().unwrap();
    assert!(!review.report.changed);
    let tex = fs::read_to_string(review.old_source.unwrap()).unwrap();
    assert!(tex.contains("\\end{figure}\n\\[x=1\\]"));
}

#[test]
fn removing_comments_preserves_the_paragraph_after_a_comment_block() {
    let f = Fixture::new(
        "First sentence.\n\nSecond sentence.",
        "First sentence.\n% An ignored comment.\n\nSecond sentence.",
    );
    let review = f.compare().unwrap();
    assert!(!review.report.changed);
    let source = fs::read_to_string(review.new_source.unwrap()).unwrap();
    assert!(source.contains("\n\nSecond sentence."));
    assert!(
        !Fixture::new("Foobar.", "Foo% ignored\nbar.")
            .compare()
            .unwrap()
            .report
            .changed
    );
}

#[test]
fn abbreviations_initials_decimals_and_quoted_endings_are_sentences() {
    let f = Fixture::new(
        "",
        "Dr. J. Smith measured 3.14 units, e.g. meters. He said `done.' Results agree.",
    );
    assert_eq!(
        texts(&f.compare().unwrap()),
        [
            "Dr. J. Smith measured 3.14 units, e.g. meters.",
            "He said `done.'",
            "Results agree."
        ]
    );
}

#[test]
fn punctuation_inside_emphasis_can_split_at_sentence_boundaries_safely() {
    let f = Fixture::new(
        r"\emph{First old sentence. Second stays.} Last stays.",
        r"\emph{First new sentence. Second stays.} Last stays.",
    );
    let review = f.compare().unwrap();
    assert_eq!(
        texts(&review),
        ["First old sentence.", "First new sentence."]
    );
    let tex = fs::read_to_string(review.source).unwrap();
    assert!(tex.contains(r"\emph{First old sentence.}"));
    assert!(tex.contains("Second stays."));
}

#[test]
fn group_delimiters_and_style_changes_are_preserved() {
    let f = Fixture::new("A normal sentence.", r"A \textbf{normal} sentence.");
    let review = f.compare().unwrap();
    assert_eq!(review.report.removed_sentences, 1);
    assert_eq!(review.report.added_sentences, 1);
    assert!(
        fs::read_to_string(review.source)
            .unwrap()
            .contains(r"\textbf{normal}")
    );
}

#[test]
fn simple_macro_changes_propagate_to_the_sentences_that_use_them() {
    let f = Fixture::new("", "");
    for (side, value) in [("old", "original"), ("new", "updated")] {
        f.document(
            side,
            &format!(r"\newcommand{{\choice}}{{{value}}}"),
            r"An \choice{} sentence. This remains.",
        );
    }
    assert_eq!(
        texts(&f.compare().unwrap()),
        ["An original sentence.", "An updated sentence."]
    );
}

#[test]
fn unused_macro_definitions_do_not_count_as_content_changes() {
    let f = Fixture::new("This remains.", "This remains.");
    f.document("new", r"\newcommand{\unused}{Invisible.}", "This remains.");
    assert!(!f.compare().unwrap().report.changed);
}

#[test]
fn parameter_optional_and_nested_macros_are_expanded_without_losing_style() {
    let f = Fixture::new(r"A \textbf{good} result.", "");
    f.document(
        "new",
        r"\newcommand{\wrap}[1]{\textbf{#1}}\newcommand{\word}[1][good]{#1}",
        r"A \wrap{\word{}} result.",
    );
    assert!(!f.compare().unwrap().report.changed);
}

#[test]
fn commented_macro_lines_do_not_add_spaces_to_replacements() {
    let f = Fixture::new("The result is foobar.", "");
    for declaration in [
        "\\newcommand{\\choice}{%\n  foobar}\n",
        "\\newcommand{\\choice}{foo% ignored newline\n  bar}\n",
    ] {
        f.document("new", declaration, r"The result is \choice{}.");
        assert!(!f.compare().unwrap().report.changed);
    }
}

#[test]
fn macros_with_internal_commands_keep_their_calls_and_track_definition_changes() {
    let body = r"\[\workrule{Val}\] This remains.";
    let f = Fixture::new(body, body);
    for (side, value) in [("old", "original"), ("new", "updated")] {
        f.document(
            side,
            &format!(
                r"\makeatletter\newcommand{{\workrule}}[1]{{\protected@edef\@currentlabel{{{value}:#1}}\downarrow}}\makeatother"
            ),
            body,
        );
    }
    let review = f.compare().unwrap();
    assert_eq!(review.report.removed_objects, 1);
    assert_eq!(review.report.added_objects, 1);
    for source in [&review.unified_source, review.new_source.as_ref().unwrap()] {
        let tex = fs::read_to_string(source).unwrap();
        let body = tex.split_once(r"\begin{document}").unwrap().1;
        assert!(body.contains(r"\workrule{Val}"));
        assert!(!body.contains(r"\protected@edef"));
    }
    let bindings = fs::read_to_string(review.assets.join("old-macros.tex")).unwrap();
    assert!(bindings.contains(r"\protected@edef\@currentlabel{original:#1}"));
}

#[test]
fn macros_with_internal_commands_in_optional_defaults_are_not_expanded() {
    let body = r"The value is $\choose$.";
    let f = Fixture::new(body, body);
    for side in ["old", "new"] {
        f.document(
            side,
            r"\makeatletter\newcommand{\internal@symbol}{\rightarrow}\newcommand{\choose}[1][\internal@symbol]{#1}\makeatother",
            body,
        );
    }
    let review = f.compare().unwrap();
    assert!(!review.report.changed);
    let tex = fs::read_to_string(review.new_source.unwrap()).unwrap();
    assert!(
        tex.split_once(r"\begin{document}")
            .unwrap()
            .1
            .contains(r"$\choose$")
    );
}

#[test]
fn math_is_opaque_to_sentence_splitting_and_normalizes_math_whitespace() {
    let f = Fixture::new(
        r"The value is $x+y=3.14$. This remains.",
        "The value is $x + y = 3.14$. This remains.",
    );
    assert!(!f.compare().unwrap().report.changed);
    let f = Fixture::new("$$x+y$$", "$$x-y$$");
    let review = f.compare().unwrap();
    assert!(review.report.changes.iter().all(|c| c.kind == Kind::Math));
}

#[test]
fn text_inside_math_preserves_significant_word_spaces() {
    let f = Fixture::new(r"$\text{two words}$.", r"$\text{twowords}$.");
    assert!(f.compare().unwrap().report.changed);
}

#[test]
fn macro_changes_inside_math_are_detected() {
    let f = Fixture::new("", "");
    for (side, value) in [("old", "x+y"), ("new", "x-y")] {
        f.document(
            side,
            &format!(r"\newcommand{{\value}}{{{value}}}"),
            r"The value is $\value$.",
        );
    }
    assert_eq!(f.compare().unwrap().report.added_sentences, 1);
}

const SPLIT_MATH_PACKAGE: &str = r"\ProvidesPackage{splitmath}
\def\unusedruntime{\input{file-that-does-not-exist}}
\newenvironment{mathpar}
  {$$\vbox\bgroup\ifmmode $\else\noindent $\displaystyle\fi}
  {\unskip\ifmmode $\fi\egroup $$\ignorespacesafterend}
";

#[test]
fn package_environment_bodies_can_store_unbalanced_math_shifts() {
    let f = Fixture::new("", "");
    for (side, value) in [("old", "1"), ("new", "2")] {
        f.write(side, "splitmath.sty", SPLIT_MATH_PACKAGE);
        f.document(
            side,
            r"\usepackage{splitmath}",
            &format!(
                r"This remains.\begin{{mathpar}}x={value}\end{{mathpar}}Entire appendix stays."
            ),
        );
    }
    let review = f.compare().unwrap();
    assert_eq!(review.report.removed_objects, 1);
    assert_eq!(review.report.added_objects, 1);
    assert!(
        review
            .report
            .changes
            .iter()
            .all(|change| change.kind == Kind::Math)
    );
    let source = fs::read_to_string(&review.source).unwrap();
    assert!(source.contains("This remains.") && source.contains("Entire appendix stays."));
    // The comparator must not execute package runtime commands or redeclare
    // unchanged package internals while rendering removed content.
    let bindings = fs::read_to_string(review.assets.join("old-macros.tex")).unwrap();
    assert!(!bindings.contains("unusedruntime") && !bindings.contains("newenvironment"));
}

#[test]
fn document_environment_declarations_do_not_require_balanced_math_bodies() {
    let f = Fixture::new("", "");
    for (side, value) in [("old", "1"), ("new", "2")] {
        f.document(
            side,
            SPLIT_MATH_PACKAGE,
            &format!(r"\begin{{mathpar}}x={value}\end{{mathpar}}"),
        );
    }
    assert!(
        latex::is_main(
            &fs::read_to_string(f.dir.path().join("new/main.tex")).unwrap(),
            Path::new("main.tex")
        )
        .unwrap()
    );
    assert_eq!(f.compare().unwrap().report.added_objects, 1);
    assert!(
        !latex::is_main(
            r"\newenvironment{fake}{\begin{document}$}{$\end{document}}",
            Path::new("not-main.tex")
        )
        .unwrap()
    );
}

#[test]
fn macro_parameters_are_substituted_before_math_is_interpreted() {
    let f = Fixture::new("The value is $x^2$. This remains.", "");
    f.document(
        "new",
        r"\newcommand{\formula}[1]{$#1^2$}",
        r"The value is \formula{x}. This remains.",
    );
    assert!(!f.compare().unwrap().report.changed);
    f.document(
        "new",
        r"\newcommand{\formula}[1]{$#1^2$}",
        r"The value is \formula{y}. This remains.",
    );
    let review = f.compare().unwrap();
    assert_eq!(review.report.added_sentences, 1);
    assert_eq!(review.report.removed_sentences, 1);
}

#[test]
fn changes_to_used_environment_definitions_are_detected() {
    let f = Fixture::new("", "");
    for (side, style) in [("old", "\\displaystyle"), ("new", "\\textstyle")] {
        f.document(
            side,
            &format!(r"\newenvironment{{mathpar}}{{\[{style}}}{{\]}}"),
            r"\begin{mathpar}x=1\end{mathpar}",
        );
    }
    let review = f.compare().unwrap();
    assert_eq!(review.report.added_objects, 1);
    assert_eq!(review.report.removed_objects, 1);
    let bindings = fs::read_to_string(review.assets.join("old-macros.tex")).unwrap();
    assert!(bindings.contains("newenvironment{mathpar}") && bindings.contains("endmathpar"));
}

#[test]
fn package_inputs_are_scanned_as_definitions_without_executing_runtime_code() {
    let f = Fixture::new("", "");
    for (side, word) in [("old", "original"), ("new", "updated")] {
        f.write(
            side,
            "local.sty",
            "\\ProvidesPackage{local}\\input{local-definitions.tex}\\endinput\n\\input{not-read.tex}",
        );
        f.write(
            side,
            "local-definitions.tex",
            &format!(
                "\\def\\runtime{{\\input{{not-present.tex}}}}\\newcommand{{\\choice}}{{{word}}}"
            ),
        );
        f.document(side, r"\usepackage{local}", r"An \choice{} sentence.");
    }
    let review = f.compare().unwrap();
    assert_eq!(
        texts(&review),
        ["An original sentence.", "An updated sentence."]
    );
}

#[test]
fn changes_to_used_environment_defaults_are_detected() {
    let f = Fixture::new("", "");
    for (side, default) in [("old", "\\displaystyle"), ("new", "\\textstyle")] {
        f.document(
            side,
            &format!(r"\newenvironment{{mathpar}}[1][{default}]{{\[#1}}{{\]}}"),
            r"\begin{mathpar}x=1\end{mathpar}",
        );
    }
    let review = f.compare().unwrap();
    assert_eq!(review.report.added_objects, 1);
    assert_eq!(review.report.removed_objects, 1);
}

#[test]
fn escaped_comments_and_verbatim_are_not_corrupted() {
    let f = Fixture::new(
        r"A \verb|100% {raw}.| sample. 20\% stays.",
        r"A \verb|50% {raw}.| sample. 20\% stays.",
    );
    let review = f.compare().unwrap();
    assert_eq!(review.report.added_sentences, 1);
    let tex = fs::read_to_string(review.source).unwrap();
    assert!(tex.contains(r"\verb|100% {raw}.|"));
    assert!(tex.contains(r"20\% stays."));
}

#[test]
fn standalone_verbatim_environment_stays_outside_macro_arguments() {
    let f = Fixture::new(
        "\\begin{verbatim}\nold % { unbalanced\n\\end{verbatim}",
        "\\begin{verbatim}\nnew % { unbalanced\n\\end{verbatim}",
    );
    assert_eq!(f.compare().unwrap().report.added_objects, 1);
}

#[test]
fn lists_and_nested_prose_environments_are_compared_recursively() {
    let f = Fixture::new(
        r"\begin{itemize}\item An old sentence.\item This stays.\end{itemize}",
        r"\begin{itemize}\item An new sentence.\item This stays.\end{itemize}",
    );
    let review = f.compare().unwrap();
    assert_eq!(texts(&review), ["An old sentence.", "An new sentence."]);
    let tex = fs::read_to_string(review.source).unwrap();
    assert_eq!(tex.matches(r"\begin{itemize}").count(), 1);
    assert_eq!(tex.matches(r"\item").count(), 2);
}

#[test]
fn complete_environment_additions_and_deletions_are_balanced() {
    let f = Fixture::new(
        r"\begin{quote}Old paragraph.\end{quote}",
        r"\begin{itemize}\item New paragraph.\end{itemize}",
    );
    let review = f.compare().unwrap();
    let tex = fs::read_to_string(review.source).unwrap();
    assert!(
        tex.contains(r"\begin{quote}")
            && tex.contains(r"\end{quote}")
            && tex.contains(r"\begin{itemize}")
            && tex.contains(r"\end{itemize}")
    );
}

#[test]
fn include_cycles_missing_files_and_recursive_macros_are_actionable_errors() {
    let f = Fixture::new("This remains.", r"\input{loop}");
    f.write("new", "loop.tex", r"\input{loop}");
    assert!(format!("{:#}", f.compare().err().unwrap()).contains("cyclic LaTeX input"));
    f.write("new", "loop.tex", r"\input{missing}");
    assert!(f.compare().is_err());
    f.document("new", r"\newcommand{\loopme}{\loopme}", r"\loopme");
    assert!(f.compare().is_err());
}

#[test]
fn pest_reports_unbalanced_groups_and_math() {
    for body in [r"An {unclosed sentence.", r"An $unclosed formula."] {
        let f = Fixture::new("This remains.", body);
        assert!(format!("{:#}", f.compare().err().unwrap()).contains("parsing LaTeX source"));
    }
}

#[test]
fn source_locations_refer_to_original_included_files() {
    let f = Fixture::new("An old sentence.", r"\input{body}");
    f.write("new", "body.tex", "% comment\nAn new sentence.\n");
    let review = f.compare().unwrap();
    let added = &review.report.changes[1];
    assert_eq!(added.locations[0].file, Path::new("body.tex"));
    assert_eq!(added.locations[0].line, 2);
}

#[test]
fn whole_document_deletion_preserves_all_deleted_contents() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("main.tex"),
        format!("{PREAMBLE}\\begin{{document}}First sentence. Second sentence.\\end{{document}}"),
    )
    .unwrap();
    let review = latex::compare(
        Some(Input {
            root: dir.path(),
            main: Path::new("main.tex"),
        }),
        None,
        &dir.path().join("out"),
        "old",
        "empty",
        Duration::from_secs(5),
    )
    .unwrap();
    assert_eq!(
        (
            review.report.removed_sentences,
            review.report.added_sentences
        ),
        (2, 0)
    );
    let tex = fs::read_to_string(review.source).unwrap();
    assert!(tex.contains("First sentence.") && tex.contains("Second sentence."));
}

#[test]
fn excessive_brace_depth_is_rejected_before_recursive_parsing() {
    let body = format!("{}Too deep.{}", "{".repeat(10_000), "}".repeat(10_000));
    let f = Fixture::new("This remains.", &body);
    assert!(format!("{:#}", f.compare().err().unwrap()).contains("128 levels"));
}

#[test]
fn comment_environments_and_macro_spacing_do_not_create_false_changes() {
    let f = Fixture::new(
        "This remains.",
        "This \\begin {comment}invisible { unbalanced\\end{comment}remains.",
    );
    assert!(!f.compare().unwrap().report.changed);
    let f = Fixture::new("First paragraph.\n\nSecond paragraph.", "");
    f.document(
        "new",
        r"\newcommand{\first}{First paragraph.}",
        "\\first\n\nSecond paragraph.",
    );
    assert!(!f.compare().unwrap().report.changed);
}

#[test]
fn font_declarations_apply_to_every_sentence_in_their_group() {
    let f = Fixture::new(
        r"{\bfseries First sentence. Second sentence.}",
        r"{\itshape First sentence. Second sentence.}",
    );
    let review = f.compare().unwrap();
    assert_eq!(review.report.added_sentences, 2);
    let old = fs::read_to_string(review.old_source.unwrap()).unwrap();
    assert!(old.contains(r"{\bfseries Second sentence.}"));
}

#[test]
fn labels_survive_markup_without_becoming_visible_content_changes() {
    let f = Fixture::new(
        r"\label{old}This stays. Next stays.",
        r"\label{new}This stays. Next stays.",
    );
    let review = f.compare().unwrap();
    assert!(!review.report.changed);
    assert!(
        fs::read_to_string(review.new_source.unwrap())
            .unwrap()
            .contains(r"\label{new}")
    );
}

#[test]
fn unused_metadata_definitions_do_not_replace_the_executable_title() {
    let f = Fixture::new("", "");
    for (side, title) in [("old", "Original title"), ("new", "Updated title")] {
        f.document(
            side,
            &format!(r"\title{{{title}}}\newcommand{{\unused}}{{\title{{Invisible title}}}}"),
            r"\maketitle",
        );
    }
    let review = f.compare().unwrap();
    assert_eq!(review.report.added_objects, 1);
    assert!(texts(&review).iter().any(|t| t.contains("Original title")));
}
