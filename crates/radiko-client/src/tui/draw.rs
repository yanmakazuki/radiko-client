fn local(time: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
    FixedOffset::east_opt(9 * 3600)
        .map(|zone| time.with_timezone(&zone))
        .unwrap_or(time)
}
fn status(p: &Program, now: DateTime<FixedOffset>) -> &'static str {
    match broadcast_status(p.key.starts_at, p.ends_at, now) {
        Ok(BroadcastStatus::Upcoming) => "放送前",
        Ok(BroadcastStatus::OnAir) => "放送中",
        Ok(BroadcastStatus::Finished) => "放送終了",
        Err(_) => "日時不正",
    }
}
fn classes(ids: &[String], empty: bool) -> String {
    let mut names = ids.to_vec();
    if empty {
        names.push("未分類".into());
    }
    if names.is_empty() {
        "指定なし".into()
    } else {
        names.join(",")
    }
}
// Display a bounded preview of common schedule HTML; this is not a browser renderer.
fn preview(text: &str) -> String {
    let mut chars = text.chars().take(12_000).peekable();
    let mut output = String::new();
    while let Some(c) = chars.next() {
        if c == '<'
            && chars
                .peek()
                .is_some_and(|next| next.is_ascii_alphabetic() || *next == '/')
        {
            let mut tag = String::new();
            let mut quoted = None;
            for value in chars.by_ref() {
                if quoted == Some(value) {
                    quoted = None;
                } else if quoted.is_none() && matches!(value, '\'' | '"') {
                    quoted = Some(value);
                } else if quoted.is_none() && value == '>' {
                    break;
                }
                tag.push(value);
            }
            let name = tag
                .trim_start_matches('/')
                .split(|c: char| c.is_whitespace() || c == '/')
                .next()
                .unwrap_or_default();
            if ["br", "p", "div", "li"]
                .iter()
                .any(|known| name.eq_ignore_ascii_case(known))
            {
                output.push('\n');
            }
        } else if !c.is_control() || matches!(c, '\n' | '\t') {
            output.push(c);
        }
    }
    for (encoded, decoded) in [
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&nbsp;", " "),
        ("&amp;", "&"),
    ] {
        output = output.replace(encoded, decoded);
    }
    output.chars().take(1200).collect()
}
fn highlighted_chapter(screen: &Screen, now: DateTime<FixedOffset>) -> Option<usize> {
    let state = &screen.app.chapters;
    let playback = screen.app.playback.as_ref()?;
    if playback.info.mode != radiko::api::stream::ProbeMode::TimeFree {
        return None;
    }
    let (bound, _) = state.jump(0).ok()?;
    if radiko::app::playback::availability(
        bound,
        radiko::api::stream::ProbeMode::TimeFree,
        now,
        Some(playback),
    ) != radiko::domain::ListeningAvailability::CurrentOutput
    {
        return None;
    }
    state
        .candidates
        .as_ref()?
        .current(playback.position?)
        .map(|chapter| chapter.source_index)
}
fn draw_chapters(frame: &mut Frame, screen: &mut Screen, area: Rect, now: DateTime<FixedOffset>) {
    let current = highlighted_chapter(screen, now);
    let state = &screen.app.chapters;
    let Some(target) = &state.target else {
        return;
    };
    let constraints = if screen.chapter_summary_focused {
        [
            Constraint::Length(7),
            Constraint::Length(3),
            Constraint::Min(1),
        ]
    } else {
        [
            Constraint::Length(7),
            Constraint::Min(1),
            Constraint::Length(3),
        ]
    };
    let panes = Layout::vertical(constraints).split(area);
    screen.chapter_height = usize::from(panes[1].height.saturating_sub(2));
    let counts = state
        .candidates
        .as_ref()
        .map(|set| {
            format!(
                "{}件 / 楽曲除外{}件 / 不正位置等{}件",
                set.chapters.len(),
                set.music_count,
                set.invalid_count
            )
        })
        .unwrap_or_default();
    let failure = if let ChapterStatus::Failed(error) = &state.status {
        error.as_str()
    } else {
        screen.chapter_notice.as_str()
    };
    frame.render_widget(Paragraph::new(format!("{} / {}\n{}〜{} JST\n{} / {}\nr 再取得 / Esc 番組詳細へ（選択移動では再生しません）\n{}",
        target.title, target.key.station_id, local(target.key.starts_at).format("%Y/%m/%d %H:%M"), local(target.ends_at).format("%Y/%m/%d %H:%M"),
        state.label(), counts, failure)).block(Block::default().borders(Borders::ALL).title("公式AIチャプター・閲覧中")), panes[0]);
    let items: Vec<_> = state
        .candidates
        .as_ref()
        .map(|set| {
            set.chapters
                .iter()
                .map(|chapter| {
                    ListItem::new(format!(
                        "{}{:.1}〜{:.1}秒 {}",
                        if current == Some(chapter.source_index) {
                            "[再生位置] "
                        } else {
                            ""
                        },
                        chapter.start.as_secs_f64(),
                        chapter.end.as_secs_f64(),
                        chapter.title
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default().borders(Borders::ALL).title(
                    if state
                        .jump(screen.chapter_list.selected().unwrap_or_default())
                        .is_ok()
                    {
                        "チャプター（Enterで再生・選択と再生位置は別表示）"
                    } else {
                        "参考位置（音声との時刻対応は未検証）"
                    },
                ),
            )
            .highlight_style(Style::default().bg(Color::DarkGray))
            .highlight_symbol("▶ "),
        panes[1],
        &mut screen.chapter_list,
    );
    let summary = screen
        .chapter_list
        .selected()
        .and_then(|index| state.candidates.as_ref()?.chapters.get(index))
        .map(|chapter| preview(&chapter.summary))
        .unwrap_or_default();
    let paragraph = Paragraph::new(summary).wrap(Wrap { trim: false });
    let scroll = &mut screen.chapter_summary_scroll;
    let lines = paragraph.line_count(panes[2].width.saturating_sub(2));
    scroll.fit(lines, panes[2].height.saturating_sub(2));
    let title = if screen.chapter_summary_focused {
        "要約（先頭1200文字まで）・上下/ページ スクロール・Tab 章一覧"
    } else {
        "選択したチャプターの要約（Tabで拡大・スクロール）"
    };
    frame.render_widget(
        paragraph.scroll((scroll.offset, 0)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(if screen.chapter_summary_focused {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                }),
        ),
        panes[2],
    );
}
fn auth_header_line(screen: &Screen) -> String {
    let summary = screen.app.authentication.status.summary();
    if screen.playback_notice.contains("認証期限切れ") {
        format!("{summary} / 再生側で認証失敗 / a 再確認")
    } else if screen.playback_notice.contains("配信認証を更新") {
        format!("{summary} / 配信認証を更新して継続")
    } else {
        summary.to_owned()
    }
}
fn draw_header(frame: &mut Frame, screen: &Screen, now: DateTime<FixedOffset>, area: Rect) {
    let scope = match screen.app.committed.scope() {
        Scope::Nationwide => "全国".into(),
        Scope::Station(id) => id.clone(),
        Scope::Areas(ids) => ids.join(","),
    };
    let filter = screen.app.committed.filter();
    let mode = if screen.live_mode
        && radiko::domain::broadcast_day(now)
            .is_some_and(|day| day != screen.app.committed.range().start())
    {
        "ライブ（取得日が当日と異なります・rで更新）"
    } else if screen.live_mode {
        "ライブ（放送中のみ）"
    } else {
        "タイムフリー"
    };
    let mode = if screen.offline {
        format!("{mode} / オフライン確認")
    } else {
        mode.to_string()
    };
    let header = format!(
        "{mode} / {} / {}〜{}（日本時間・05:00区切り）\nキーワード: {}\n番組ジャンル: {} / 出演者分類: {}\n認証: {}\n{}",
        scope,
        screen.app.committed.range().start(),
        screen.app.committed.range().end(),
        filter.keyword().unwrap_or("指定なし"),
        classes(filter.program_genres(), filter.unclassified_program()),
        classes(
            filter.personality_genres(),
            filter.unclassified_personality()
        ),
        auth_header_line(screen),
        screen.app.authentication.status.detail()
    );
    frame.render_widget(
        Paragraph::new(header).block(
            Block::default()
                .title("radiko-client")
                .borders(Borders::ALL),
        ),
        area,
    );
}
fn draw_programs(frame: &mut Frame, screen: &mut Screen, now: DateTime<FixedOffset>, area: Rect) {
    let panes =
        Layout::horizontal([Constraint::Percentage(48), Constraint::Percentage(52)]).split(area);
    screen.list_height = usize::from(panes[0].height.saturating_sub(2));
    let count = screen.rows().len();
    let visible = viewport::visible(&mut screen.list, count, screen.list_height);
    let mut local_list = ListState::default();
    local_list.select(
        screen
            .list
            .selected()
            .filter(|i| visible.contains(i))
            .map(|i| i - visible.start),
    );
    let items: Vec<_> = screen.rows()[visible]
        .iter()
        .map(|i| {
            let p = &screen.app.programs[*i];
            ListItem::new(format!(
                "{} {} [{}] {}",
                local(p.key.starts_at).format("%m/%d %H:%M"),
                p.key.station_id,
                status(p, now),
                p.title.chars().take(120).collect::<String>()
            ))
        })
        .collect();
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title(format!(
                "番組一覧 一致{} / 取得{}枠",
                screen.rows().len(),
                screen.app.programs.len()
            )))
            .highlight_style(Style::default().bg(Color::DarkGray))
            .highlight_symbol("▶ "),
        panes[0],
        &mut local_list,
    );
    let mut detail = screen
        .selected()
        .map(|p| {
            format!(
                "{}\n{} / {}〜{} JST\n{} / {}\n出演: {}\n\n{}\n{}",
                p.title,
                p.key.station_id,
                local(p.key.starts_at).format("%m/%d %H:%M"),
                local(p.ends_at).format("%m/%d %H:%M"),
                status(p, now),
                radiko::app::playback::availability(
                    p,
                    if screen.live_mode {
                        radiko::api::stream::ProbeMode::Live
                    } else {
                        radiko::api::stream::ProbeMode::TimeFree
                    },
                    now,
                    screen.app.playback.as_ref(),
                )
                .label(),
                p.performer,
                preview(&p.description),
                preview(&p.info)
            )
        })
        .unwrap_or_else(|| "選択できる番組はありません".into());
    for failure in screen
        .app
        .area_failures
        .iter()
        .chain(&screen.app.schedule_failures)
        .take(5)
    {
        detail.push_str(&format!(
            "\n未取得: {} {} {}",
            failure.target,
            failure.day.map(|d| d.to_string()).unwrap_or_default(),
            failure.error
        ));
    }
    frame.render_widget(
        Paragraph::new(detail).wrap(Wrap { trim: false }).block(
            Block::default()
                .title("番組詳細（閲覧中）")
                .borders(Borders::ALL),
        ),
        panes[1],
    );
}
fn draw_status(frame: &mut Frame, screen: &Screen, area: Rect, show_http: bool) {
    let state = match screen.app.status {
        SearchStatus::Idle => "未取得",
        SearchStatus::Loading => "取得中",
        SearchStatus::Ready => "取得完了",
        SearchStatus::Partial => "部分取得・未取得あり",
        SearchStatus::Failed => "取得失敗",
    };
    let progress = screen
        .app
        .progress
        .as_ref()
        .map(|p| {
            format!(
                "{} {}/{} 失敗{} / {} {}",
                p.phase,
                p.completed,
                p.total,
                p.failed,
                p.target,
                p.day.map(|d| d.to_string()).unwrap_or_default()
            )
        })
        .unwrap_or_default();
    let playing = screen
        .app
        .playback
        .as_ref()
        .map(|p| {
            format!(
                "再生中: [{} / {:?}] {}\n{}〜{} JST / {:?} / 音量{}% / {}",
                p.info.station,
                p.info.mode,
                p.info.title.as_deref().unwrap_or("番組名未取得"),
                local(p.info.starts_at).format("%m/%d %H:%M"),
                local(p.info.ends_at).format("%m/%d %H:%M"),
                p.state,
                p.volume,
                p.position
                    .map(|v| format!("位置{:.1}秒", v.as_secs_f64()))
                    .unwrap_or_else(|| "番組内位置未確認".into())
            )
        })
        .unwrap_or_else(|| format!("再生中: なし / 音量{}%", screen.volume));
    let controls = screen.control_hint();
    let pipeline = screen
        .app
        .playback
        .as_ref()
        .and_then(|p| p.pipeline.as_ref())
        .map(|p| p.lines())
        .unwrap_or_else(|| {
            if screen.pipeline_diagnostics.is_empty() {
                "処理診断 未取得\n通信診断 未取得".into()
            } else {
                screen.pipeline_diagnostics.clone()
            }
        });
    let pipeline = if show_http {
        pipeline
    } else {
        pipeline
            .lines()
            .next()
            .unwrap_or("処理診断 未取得")
            .to_owned()
    };
    let footer = format!(
        "{} / {} / 不正枠{}\n{}\n{}\n{}\n{}\n{}\n{} {}",
        state,
        progress,
        screen.app.invalid_count,
        playing,
        screen.playback_diagnostics,
        pipeline,
        controls,
        screen.playback_notice,
        screen
            .filters
            .as_ref()
            .map(|editor| editor.error.as_str())
            .unwrap_or(&screen.app.notification),
        screen.preferences_notice
    );
    frame.render_widget(
        Paragraph::new(footer).block(Block::default().borders(Borders::ALL)),
        area,
    );
}
fn draw(frame: &mut Frame, screen: &mut Screen, now: DateTime<FixedOffset>) {
    if screen.failures_open {
        failures::draw(
            frame,
            &screen.app,
            &mut screen.failure_list,
            &mut screen.failure_height,
        );
        return;
    }
    if screen.help_open {
        help::draw(
            frame,
            screen.app.playback.as_ref(),
            screen.volume,
            &mut screen.help_scroll,
            &screen.pipeline_diagnostics,
        );
        return;
    }
    let show_http = frame.area().height >= 36;
    let areas = Layout::vertical([
        Constraint::Length(7),
        Constraint::Min(1),
        Constraint::Length(if screen.input.is_some() {
            4
        } else if screen.seek.is_some() {
            11 + u16::from(show_http)
        } else {
            10 + u16::from(show_http)
        }),
    ])
    .split(frame.area());
    draw_header(frame, screen, now, areas[0]);
    if let Some(editor) = &screen.login {
        editor.draw(frame, areas[1]);
    } else if let Some(manager) = &mut screen.named {
        manager.draw(frame, areas[1], &screen.app.committed);
    } else if let Some(editor) = &mut screen.filters {
        editor.draw(frame, areas[1]);
    } else if screen.app.chapters.target.is_some() {
        draw_chapters(frame, screen, areas[1], now);
    } else {
        draw_programs(frame, screen, now, areas[1]);
    }
    if let Some(input) = &screen.input {
        frame.render_widget(
            Paragraph::new(format!(
                "キーワード: {input}\nEnter 検索 / Ctrl+U 消去 / Esc 取消（編集中は取得しません）"
            ))
            .block(Block::default().borders(Borders::ALL).title("条件編集中")),
            areas[2],
        );
    } else {
        draw_status(frame, screen, areas[2], show_http);
    }
}
