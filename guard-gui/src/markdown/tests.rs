use super::*;

fn plain(text: &str) -> Span {
    Span {
        text: text.into(),
        code: false,
        bold: false,
    }
}

fn code(text: &str) -> Span {
    Span {
        text: text.into(),
        code: true,
        bold: false,
    }
}

fn bold(text: &str) -> Span {
    Span {
        text: text.into(),
        code: false,
        bold: true,
    }
}

#[test]
fn headings_keep_their_level() {
    assert_eq!(
        blocks("# Title\n## checkout.guest\n### Detail"),
        vec![
            Block::Heading(1, vec![plain("Title")]),
            Block::Heading(2, vec![plain("checkout.guest")]),
            Block::Heading(3, vec![plain("Detail")]),
        ]
    );
}

#[test]
fn a_hash_without_a_space_is_text() {
    assert_eq!(
        blocks("#hashtag"),
        vec![Block::Paragraph(vec![plain("#hashtag")])]
    );
}

#[test]
fn dash_and_star_bullets_are_bullets() {
    assert_eq!(
        blocks("- one\n* two\n  - nested"),
        vec![
            Block::Bullet(vec![plain("one")]),
            Block::Bullet(vec![plain("two")]),
            Block::Bullet(vec![plain("nested")]),
        ]
    );
}

#[test]
fn consecutive_lines_join_into_one_paragraph_and_blank_lines_split() {
    assert_eq!(
        blocks("first line\nsecond line\n\nnext"),
        vec![
            Block::Paragraph(vec![plain("first line second line")]),
            Block::Paragraph(vec![plain("next")]),
        ]
    );
}

#[test]
fn inline_code_and_bold_become_styled_spans() {
    assert_eq!(
        spans("El total `order-total` falla en **checkout**."),
        vec![
            plain("El total "),
            code("order-total"),
            plain(" falla en "),
            bold("checkout"),
            plain("."),
        ]
    );
}

#[test]
fn stars_inside_code_stay_literal_and_an_unclosed_backtick_is_text() {
    assert_eq!(spans("`a ** b`"), vec![code("a ** b")]);
    assert_eq!(spans("it's `open"), vec![plain("it's `open")]);
}

#[test]
fn fenced_code_is_one_verbatim_block() {
    assert_eq!(
        blocks("Error:\n```\nexpect(x)\n  .toBe(1)\n```\nAfter"),
        vec![
            Block::Paragraph(vec![plain("Error:")]),
            Block::Code("expect(x)\n  .toBe(1)".into()),
            Block::Paragraph(vec![plain("After")]),
        ]
    );
}

#[test]
fn numbered_items_stay_on_their_own_line() {
    assert_eq!(
        blocks("1. first\n2. second"),
        vec![
            Block::Paragraph(vec![plain("1. first")]),
            Block::Paragraph(vec![plain("2. second")]),
        ]
    );
}

#[test]
fn empty_input_has_no_blocks() {
    assert!(blocks("").is_empty());
    assert!(blocks("\n  \n").is_empty());
}

#[test]
fn a_pipe_table_becomes_a_table_block() {
    let md = "# guard explain — 2026-11-05 · m\n\n\
              | # | Test | Qué pasó |\n\
              | --- | --- | --- |\n\
              | 1 | Login | a \\| b<br>second |\n\
              | 2 | `cart` | **Bug.** detail |\n\n\
              **Veredicto:** real bug";
    assert_eq!(
        blocks(md),
        vec![
            Block::Heading(1, vec![plain("guard explain — 2026-11-05 · m")]),
            Block::Table {
                header: vec![
                    vec![plain("#")],
                    vec![plain("Test")],
                    vec![plain("Qué pasó")]
                ],
                rows: vec![
                    vec![
                        vec![plain("1")],
                        vec![plain("Login")],
                        vec![plain("a | b\nsecond")],
                    ],
                    vec![
                        vec![plain("2")],
                        vec![code("cart")],
                        vec![bold("Bug."), plain(" detail")],
                    ],
                ],
            },
            Block::Paragraph(vec![bold("Veredicto:"), plain(" real bug")]),
        ]
    );
}

#[test]
fn pipe_lines_without_a_separator_stay_text() {
    assert_eq!(
        blocks("| not | a table |\nnext"),
        vec![Block::Paragraph(vec![plain("| not | a table | next")])]
    );
}

#[test]
fn a_short_table_row_is_padded_to_the_header() {
    let Block::Table { rows, .. } = &blocks("| a | b |\n|---|:-:|\n| 1 |")[0] else {
        panic!("expected a table");
    };
    assert_eq!(rows[0], vec![vec![plain("1")], vec![]]);
}
