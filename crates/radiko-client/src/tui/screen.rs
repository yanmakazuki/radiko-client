use super::*;
impl Screen {
    pub(super) fn new(criteria: SearchCriteria, preferences_notice: String) -> Self {
        Self {
            help_open: false,
            help_scroll: scroll::Scroll::default(),
            failures_open: false,
            failure_list: ListState::default(),
            failure_height: 1,
            login: None,
            login_requested: None,
            logout_confirm: false,
            logout_requested: false,
            live_mode: false,
            live_rows: vec![],
            live_refresh_at: None,
            last_live_time: None,
            timefree_criteria: None,
            seek: None,
            named: None,
            named_action: None,
            named_generation: 0,
            filters: None,
            auth_refresh: false,
            chapter_action: None,
            chapter_list: ListState::default(),
            chapter_height: 1,
            chapter_summary_focused: false,
            chapter_summary_scroll: scroll::Scroll::default(),
            chapter_notice: String::new(),
            playback_action: None,
            playback_notice: String::new(),
            notice_hold: super::NoticeHold::Other,
            notice_hold_until: None,
            notice_clock: None,
            playback_stopped: false,
            playback_ended: false,
            seen_reauthentications: 0,
            seen_skipped_segments: 0,
            seen_sequence_resets: 0,
            playback_diagnostics: String::new(),
            pipeline_diagnostics: String::new(),
            volume: 25,
            app: AppState::new(criteria),
            list: ListState::default(),
            list_height: 1,
            input: None,
            preferences_notice,
            offline: false,
        }
    }
    pub(super) fn dispatch_requested(
        &mut self,
        player: &mut PlaybackService,
        authentication: &mut AuthService,
        login_service: &mut LoginService,
        logout: &mut LogoutService,
        chapters: &mut ChapterService,
    ) -> Result<(), &'static str> {
        if let Some(credentials) = self.login_requested.take() {
            if self.offline {
                drop(credentials);
                self.show_notice("オフライン確認ではログインしません");
            } else {
                self.auth_refresh = false;
                authentication.cancel();
                login_service.submit(self.app.authentication.begin_login()?, credentials)?;
            }
        }
        if std::mem::take(&mut self.logout_requested) {
            player.stop();
            self.playback_action = None;
            self.auth_refresh = false;
            authentication.cancel();
            logout.submit(self.app.authentication.begin_logout()?)?;
        }
        if std::mem::take(&mut self.auth_refresh) {
            if self.offline {
                self.show_notice("オフライン確認ではセッションを通信確認しません");
            } else {
                authentication.submit(self.app.authentication.begin()?);
            }
        }
        if let Some(action) = self.chapter_action.take() {
            match action {
                ChapterAction::Fetch(_, _) if self.offline => {
                    self.chapter_notice = "オフライン確認ではチャプターを取得しません".into();
                }
                ChapterAction::Fetch(id, target) => chapters.submit(id, *target),
                ChapterAction::Cancel => chapters.cancel(),
            }
        }
        self.dispatch_playback(player, Utc::now().fixed_offset());
        Ok(())
    }
    pub(super) fn dispatch_playback(
        &mut self,
        player: &mut PlaybackService,
        now: DateTime<FixedOffset>,
    ) {
        let Some(action) = self.playback_action.take() else {
            return;
        };
        if self.offline
            && !matches!(
                action,
                PlaybackAction::Stop | PlaybackAction::Command(Command::Volume(_))
            )
        {
            self.show_notice("オフライン確認では再生しません");
            return;
        }
        let volume = match &action {
            PlaybackAction::Command(Command::Volume(volume)) => Some((*volume).min(100)),
            _ => None,
        };
        let result = match action {
            PlaybackAction::Select(program) => player.select(&program, now),
            PlaybackAction::Chapter(program, position) => player.select_at(&program, position, now),
            PlaybackAction::Live(program) => player.select_live(&program, now),
            PlaybackAction::Command(command) => player.command(command),
            PlaybackAction::Stop => {
                player.stop();
                Ok(())
            }
        };
        match result {
            Ok(()) => {
                if let Some(volume) = volume {
                    self.volume = volume;
                }
            }
            Err(reason) => self.show_notice(reason),
        }
    }
    fn playback_now(&self) -> std::time::Instant {
        self.notice_clock.unwrap_or_else(std::time::Instant::now)
    }
    pub(super) fn apply_playback(&mut self, update: playback::Update) {
        self.apply_playback_at(update, self.playback_now());
    }
    #[cfg(test)]
    pub(super) fn set_notice_clock(&mut self, now: std::time::Instant) {
        self.notice_clock = Some(now);
    }
    pub(super) fn apply_playback_at(&mut self, update: playback::Update, now: std::time::Instant) {
        if update.new_session {
            self.app.playback = None;
            self.playback_diagnostics.clear();
            self.pipeline_diagnostics.clear();
            self.playback_notice.clear();
            self.notice_hold = super::NoticeHold::Other;
            self.notice_hold_until = None;
            self.playback_stopped = false;
            self.playback_ended = false;
            self.seen_reauthentications = 0;
            self.seen_skipped_segments = 0;
            self.seen_sequence_resets = 0;
        }
        if update.stopped {
            self.playback_stopped = true;
        }
        if update.ended {
            self.playback_ended = true;
        }
        if let Some(snapshot) = &update.snapshot {
            self.pipeline_diagnostics = snapshot
                .pipeline
                .as_ref()
                .map(|p| p.lines())
                .unwrap_or_default();
            self.playback_diagnostics = radiko::player::state::output_diagnostics(
                snapshot.output.as_ref(),
                snapshot.segments,
            );
        }
        if let Some(diagnostics) = update.final_diagnostics {
            self.playback_diagnostics = format!("停止時: {diagnostics}");
        } else if update.ended && !self.playback_diagnostics.is_empty() {
            self.playback_diagnostics =
                format!("直近通知（終了時未集計）: {}", self.playback_diagnostics);
        }
        let has_snapshot = update.snapshot.is_some();
        let reconnecting_for = update
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.reconnecting_for);
        let receiving = update
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.receiving_for.is_some());
        let snapshot_state = update.snapshot.as_ref().map(|snapshot| snapshot.state);
        let http = update
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.pipeline.as_ref())
            .and_then(|pipeline| pipeline.http);
        let renewed = http
            .map(|http| http.reauthentications)
            .filter(|count| *count > self.seen_reauthentications);
        let realign = http
            .map(|http| (http.skipped_segments, http.sequence_resets))
            .filter(|(skipped, resets)| {
                *resets > self.seen_sequence_resets || *skipped > self.seen_skipped_segments
            });
        if update.ended {
            if !self.pipeline_diagnostics.is_empty() {
                self.pipeline_diagnostics = self
                    .pipeline_diagnostics
                    .lines()
                    .map(|line| format!("直近通知（終了時未集計）: {line}"))
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            self.app.playback = None;
        } else if let Some(snapshot) = update.snapshot {
            self.app.playback = Some(snapshot);
        }
        if let Some(count) = renewed {
            self.seen_reauthentications = count;
            if !self.playback_stopped && !self.playback_ended {
                self.playback_notice = "配信認証を更新して再生を続けます".into();
                self.notice_hold = super::NoticeHold::AuthRenewal;
                self.notice_hold_until = Some(now + super::NOTICE_HOLD);
            }
        }
        let at_head = http.is_some_and(|http| http.joined_window_head);
        let paused = snapshot_state == Some(radiko::player::state::PlaybackState::Paused);
        let before_audio = snapshot_state == Some(radiko::player::state::PlaybackState::Buffering);
        let held = self.notice_hold_until.is_some_and(|until| now < until)
            && matches!(
                self.notice_hold,
                super::NoticeHold::AuthRenewal
                    | super::NoticeHold::CatchUp
                    | super::NoticeHold::Action
            );
        // An empty poll has no snapshot. Leaving the notice alone stops the
        // once-a-second blink: most 50ms ticks carry no playback snapshot.
        if has_snapshot {
            let show_gap =
                !self.playback_stopped && !self.playback_ended && !paused && !before_audio && !held;
            if show_gap {
                if let Some(elapsed) = reconnecting_for {
                    self.playback_notice = radiko::player::playback::reconnect_notice(elapsed);
                    self.notice_hold = super::NoticeHold::Reconnecting;
                    self.notice_hold_until = None;
                } else if receiving {
                    self.playback_notice = radiko::player::playback::RECEPTION_NOTICE.into();
                    self.notice_hold = super::NoticeHold::Receiving;
                    self.notice_hold_until = None;
                } else if matches!(
                    self.notice_hold,
                    super::NoticeHold::Receiving | super::NoticeHold::Reconnecting
                ) {
                    self.playback_notice.clear();
                    self.notice_hold = super::NoticeHold::Other;
                    self.notice_hold_until = None;
                }
            } else if !held
                && matches!(
                    self.notice_hold,
                    super::NoticeHold::Receiving | super::NoticeHold::Reconnecting
                )
            {
                self.playback_notice.clear();
                self.notice_hold = super::NoticeHold::Other;
                self.notice_hold_until = None;
            }
        }
        if let Some((skipped, resets)) = realign {
            self.seen_sequence_resets = resets;
            self.seen_skipped_segments = skipped;
            let protected = held
                && matches!(
                    self.notice_hold,
                    super::NoticeHold::AuthRenewal | super::NoticeHold::Action
                );
            if !self.playback_stopped && !self.playback_ended && !protected {
                self.playback_notice = if at_head {
                    format!("配信窓の先頭から再開しました（累計{skipped}）")
                } else {
                    format!("ライブ端に追いつきました（飛ばしたセグメントは累計{skipped}）")
                };
                self.notice_hold = super::NoticeHold::CatchUp;
                self.notice_hold_until = Some(now + super::NOTICE_HOLD);
            }
        }
        if let Some(notice) = update.notice {
            self.playback_notice = notice;
            if self.playback_stopped || self.playback_ended {
                self.notice_hold = super::NoticeHold::Other;
                self.notice_hold_until = None;
            } else {
                self.hold_action(now);
            }
        }
    }
    fn hold_action(&mut self, now: std::time::Instant) {
        self.notice_hold = super::NoticeHold::Action;
        self.notice_hold_until = Some(now + super::NOTICE_HOLD);
    }
    pub(super) fn show_notice(&mut self, notice: impl Into<String>) {
        let now = self.playback_now();
        self.playback_notice = notice.into();
        self.hold_action(now);
    }
    fn has_footer_notice(&self) -> bool {
        !self.playback_notice.is_empty()
            || !self.app.notification.is_empty()
            || !self.preferences_notice.is_empty()
    }
    /// 通常画面の Esc。再生・一時停止・選択・条件と停止フラグは残す。
    fn dismiss_notices(&mut self) {
        self.playback_notice.clear();
        self.app.notification.clear();
        self.preferences_notice.clear();
        if matches!(
            self.notice_hold,
            super::NoticeHold::Action | super::NoticeHold::CatchUp | super::NoticeHold::AuthRenewal
        ) {
            self.notice_hold = super::NoticeHold::Other;
            self.notice_hold_until = None;
        }
    }
    pub(super) fn rows(&self) -> &[usize] {
        if self.live_mode {
            &self.live_rows
        } else {
            &self.app.visible
        }
    }
    pub(super) fn rebuild_live(&mut self, now: DateTime<FixedOffset>) {
        self.last_live_time = Some(now);
        self.live_rows = self
            .app
            .visible
            .iter()
            .copied()
            .filter(|i| {
                let p = &self.app.programs[*i];
                p.key.starts_at <= now && now < p.ends_at
            })
            .collect();
        self.live_refresh_at = self
            .app
            .visible
            .iter()
            .flat_map(|i| {
                let p = &self.app.programs[*i];
                [p.key.starts_at, p.ends_at]
            })
            .filter(|time| *time > now)
            .min();
    }
    pub(super) fn restore_selection(&mut self, key: Option<radiko::domain::ProgramKey>) {
        let position = key.and_then(|key| {
            self.rows()
                .iter()
                .position(|i| self.app.programs[*i].key == key)
        });
        let fallback = self
            .list
            .selected()
            .unwrap_or_default()
            .min(self.rows().len().saturating_sub(1));
        let selected = if self.rows().is_empty() {
            None
        } else {
            Some(position.unwrap_or(fallback))
        };
        self.list.select(selected);
    }
    pub(super) fn refresh_live(&mut self, now: DateTime<FixedOffset>) {
        if !self.live_mode {
            return;
        }
        if self.live_refresh_at.is_some_and(|time| now >= time)
            || self.last_live_time.is_some_and(|time| now < time)
        {
            let key = self.selected().map(|p| p.key.clone());
            self.rebuild_live(now);
            self.restore_selection(key);
        }
        self.last_live_time = Some(now);
    }
    pub(super) fn toggle_live(&mut self, service: &mut Service) -> Result<(), &'static str> {
        let criteria = if self.live_mode {
            self.timefree_criteria
                .take()
                .unwrap_or_else(|| self.app.committed.clone())
        } else {
            let criteria = self
                .app
                .committed
                .restore_startup(Utc::now().fixed_offset())?;
            self.timefree_criteria = Some(self.app.committed.clone());
            criteria
        };
        self.live_mode = !self.live_mode;
        self.app.begin_edit();
        self.app.edit_criteria(criteria)?;
        self.submit(service)
    }
    pub(super) fn selected(&self) -> Option<&Program> {
        self.list
            .selected()
            .and_then(|i| self.rows().get(i))
            .and_then(|i| self.app.programs.get(*i))
    }
    pub(super) fn submit(&mut self, service: &mut Service) -> Result<(), &'static str> {
        if self.app.chapters.target.is_some() {
            self.close_chapters();
        }
        if self.live_mode {
            let criteria = self
                .app
                .draft
                .as_ref()
                .unwrap_or(&self.app.committed)
                .restore_startup(Utc::now().fixed_offset())?;
            if self
                .app
                .draft
                .as_ref()
                .is_some_and(|draft| draft.range() != criteria.range())
            {
                // Explicit saved dates and form edits keep their meaning in the time-free view.
                self.live_mode = false;
                self.timefree_criteria = None;
            } else {
                self.app.begin_edit();
                self.app.edit_criteria(criteria)?;
            }
        }
        let id = self.app.commit()?;
        self.live_rows.clear();
        self.live_refresh_at = None;
        self.last_live_time = None;
        self.list.select(None);
        service.submit(id, self.app.committed.clone());
        Ok(())
    }
    pub(super) fn apply(&mut self, event: radiko::app::search::Event) {
        let key = self.selected().map(|p| p.key.clone());
        self.app.apply(event);
        self.rebuild_live(Utc::now().fixed_offset());
        self.restore_selection(key);
    }
    pub(super) fn open_chapters(&mut self, target: Program) -> Result<(), &'static str> {
        let id = self.app.chapters.open(target.clone())?;
        self.chapter_list.select(None);
        self.chapter_summary_focused = false;
        self.chapter_summary_scroll = scroll::Scroll::default();
        self.chapter_notice.clear();
        self.chapter_action = Some(ChapterAction::Fetch(id, Box::new(target)));
        Ok(())
    }
    fn close_chapters(&mut self) {
        self.app.chapters.close();
        self.chapter_list.select(None);
        self.chapter_summary_focused = false;
        self.chapter_summary_scroll = scroll::Scroll::default();
        self.chapter_notice.clear();
        self.chapter_action = Some(ChapterAction::Cancel);
    }
    pub(super) fn named_event(&mut self, event: NamedEvent) -> Result<(), &'static str> {
        if event.mutating {
            self.preferences_notice = named_receipt(&event.result);
        }
        let Some(manager) = &mut self.named else {
            return Ok(());
        };
        if manager.view != event.view {
            if manager.busy {
                self.named_action = Some(NamedOperation::List);
            }
            return Ok(());
        }
        manager.busy = false;
        match event.result {
            Ok(NamedOutcome::Listed(items)) => manager.listed(items),
            Ok(NamedOutcome::Saved(_) | NamedOutcome::Deleted(_)) => {
                manager.busy = true;
                self.named_action = Some(NamedOperation::List);
            }
            Ok(NamedOutcome::Recalled(criteria)) => {
                self.app.begin_edit();
                self.app.edit_criteria((*criteria).clone())?;
                self.filters = Some(filters::Editor::new(&criteria, &self.app.programs));
                self.named = None;
            }
            Err(error) => manager.notice = error.to_string(),
        }
        Ok(())
    }
    pub(super) fn dispatch_named(&mut self, service: &mut NamedService) {
        if let Some(operation) = self.named_action.take() {
            if let Some(manager) = &mut self.named {
                if let Err(error) = service.submit(manager.view, operation) {
                    manager.notice = error.into();
                }
            }
        }
    }
    pub(super) fn key(
        &mut self,
        key: KeyEvent,
        service: &mut Service,
    ) -> Result<bool, &'static str> {
        if key.kind == KeyEventKind::Release
            || (key.kind == KeyEventKind::Repeat && key.code == KeyCode::Enter)
        {
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(true);
        }
        if self.help_open {
            return self.key_help(key);
        }
        if self.failures_open {
            return self.key_failures(key);
        }
        if self.logout_confirm {
            return self.key_logout(key);
        }
        if self.login.is_some() {
            return self.key_login(key);
        }
        if self.seek.is_some() {
            return self.key_seek(key);
        }
        if self.named.is_some() {
            return self.key_named(key);
        }
        if self.filters.is_some() {
            return self.key_filters(key, service);
        }
        if self.input.is_some() {
            return self.key_keyword(key, service);
        }
        if let Some(stopped) = self.key_while_authenticating(key) {
            return Ok(stopped);
        }
        if let Some(stopped) = self.key_chapters(key)? {
            return Ok(stopped);
        }
        self.key_browser(key, service)
    }
    fn key_help(&mut self, key: KeyEvent) -> Result<bool, &'static str> {
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('?' | 'q')) {
            self.help_open = false;
        } else {
            self.help_scroll.key(key.code);
        }
        Ok(false)
    }
    fn key_failures(&mut self, key: KeyEvent) -> Result<bool, &'static str> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('e' | 'q') => self.failures_open = false,
            key => viewport::navigate(
                &mut self.failure_list,
                failures::count(&self.app),
                self.failure_height,
                key,
            ),
        }
        Ok(false)
    }
    fn key_logout(&mut self, key: KeyEvent) -> Result<bool, &'static str> {
        match key.code {
            KeyCode::Char('y') => {
                self.logout_confirm = false;
                if self.offline {
                    self.show_notice("オフライン確認ではログアウトしません");
                } else {
                    self.logout_requested = true;
                }
            }
            KeyCode::Char('n') | KeyCode::Esc | KeyCode::Char('q') => self.logout_confirm = false,
            _ => (),
        }
        Ok(false)
    }
    fn key_login(&mut self, key: KeyEvent) -> Result<bool, &'static str> {
        if let Some(editor) = &mut self.login {
            match editor.handle(key) {
                login::Action::Close => self.login = None,
                login::Action::Submit(credentials) => {
                    self.login_requested = Some(credentials);
                    self.login = None;
                }
                login::Action::None => (),
            }
        }
        Ok(false)
    }
    fn key_seek(&mut self, key: KeyEvent) -> Result<bool, &'static str> {
        if let Some(editor) = &mut self.seek {
            match editor.handle(key) {
                seek::Action::Close => self.seek = None,
                seek::Action::Submit(command) => {
                    self.playback_action = Some(PlaybackAction::Command(command));
                    self.seek = None;
                }
                seek::Action::None => (),
            }
        }
        Ok(false)
    }
    fn key_named(&mut self, key: KeyEvent) -> Result<bool, &'static str> {
        if let Some(manager) = &mut self.named {
            match manager.handle(key, &self.app.committed) {
                named::Action::Close => self.named = None,
                named::Action::Operation(operation) => self.named_action = Some(operation),
                named::Action::None => (),
            }
        }
        Ok(false)
    }
    fn key_filters(&mut self, key: KeyEvent, service: &mut Service) -> Result<bool, &'static str> {
        if let Some(editor) = &mut self.filters {
            match editor.handle(key) {
                filters::Action::Cancel => {
                    self.filters = None;
                    self.app.cancel_edit();
                }
                filters::Action::Submit => match editor.criteria() {
                    Ok(criteria) => {
                        self.app.edit_criteria(criteria)?;
                        self.submit(service)?;
                        self.filters = None;
                    }
                    Err(reason) => editor.error = reason.into(),
                },
                filters::Action::Edit => {
                    editor.error.clear();
                    if let Ok(criteria) = editor.criteria() {
                        self.app.edit_criteria(criteria)?;
                    }
                }
                filters::Action::None => (),
            }
        }
        Ok(false)
    }
    fn key_keyword(&mut self, key: KeyEvent, service: &mut Service) -> Result<bool, &'static str> {
        if let Some(input) = self.input.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    self.input = None;
                    self.app.cancel_edit();
                }
                KeyCode::Enter => {
                    let keyword = (!input.trim().is_empty()).then(|| input.clone());
                    match self.app.edit_keyword(keyword) {
                        Ok(()) => {
                            self.input = None;
                            self.submit(service)?;
                        }
                        Err(error) => self.app.notification = error.into(),
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.clear()
                }
                KeyCode::Char(c)
                    if !c.is_control()
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && input.len() + c.len_utf8() <= 1024 =>
                {
                    input.push(c);
                }
                _ => (),
            }
        }
        if let Some(input) = &self.input {
            self.app
                .edit_keyword((!input.trim().is_empty()).then(|| input.clone()))?;
        }
        Ok(false)
    }
    fn key_while_authenticating(&mut self, key: KeyEvent) -> Option<bool> {
        if matches!(
            self.app.authentication.status,
            AuthStatus::LoggingOut | AuthStatus::LoggingIn
        ) && matches!(key.code, KeyCode::Char('I' | 'O' | 'a'))
        {
            self.show_notice("認証情報の更新処理中です");
            return Some(false);
        }
        if matches!(self.app.authentication.status, AuthStatus::LoggingOut)
            && matches!(
                key.code,
                KeyCode::Enter
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Char('a' | 'O' | 'l' | 'g' | ' ')
            )
        {
            self.show_notice("ログアウト処理中です");
            return Some(false);
        }
        None
    }
    fn key_chapters(&mut self, key: KeyEvent) -> Result<Option<bool>, &'static str> {
        if self.app.chapters.target.is_none() {
            return Ok(None);
        }
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.chapter_summary_focused = !self.chapter_summary_focused;
                Ok(Some(false))
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.close_chapters();
                Ok(Some(false))
            }
            KeyCode::Char('r') | KeyCode::Char('c') => {
                if let Some(target) = self.app.chapters.target.clone() {
                    self.open_chapters(target)?;
                }
                Ok(Some(false))
            }
            KeyCode::Down
            | KeyCode::Char('j')
            | KeyCode::Up
            | KeyCode::Char('k')
            | KeyCode::PageDown
            | KeyCode::PageUp
            | KeyCode::Home
            | KeyCode::End => {
                let count = self
                    .app
                    .chapters
                    .candidates
                    .as_ref()
                    .map_or(0, |set| set.chapters.len());
                if self.chapter_summary_focused {
                    self.chapter_summary_scroll.key(key.code);
                } else {
                    let previous = self.chapter_list.selected();
                    viewport::navigate(
                        &mut self.chapter_list,
                        count,
                        self.chapter_height,
                        key.code,
                    );
                    if self.chapter_list.selected() != previous {
                        self.chapter_summary_scroll = scroll::Scroll::default();
                    }
                }
                Ok(Some(false))
            }
            KeyCode::Enter => {
                self.confirm_chapter()?;
                Ok(Some(false))
            }
            KeyCode::Char('/') | KeyCode::Char('f') | KeyCode::Char('b') => Ok(Some(false)),
            _ => Ok(None),
        }
    }
    fn confirm_chapter(&mut self) -> Result<(), &'static str> {
        let index = self.chapter_list.selected().unwrap_or_default();
        match self.app.chapters.jump(index) {
            Ok((program, position)) => {
                match radiko::app::playback::request(program, Utc::now().fixed_offset()) {
                    Err(reason) => self.chapter_notice = reason.into(),
                    Ok(_) => {
                        let same = self.app.playback.as_ref().is_some_and(|playback| {
                            playback.info.station == program.key.station_id
                                && playback.info.starts_at == program.key.starts_at
                                && playback.info.mode == radiko::api::stream::ProbeMode::TimeFree
                        });
                        self.playback_action = Some(if same {
                            PlaybackAction::Command(Command::SeekSlot {
                                key: program.key.clone(),
                                position,
                            })
                        } else {
                            PlaybackAction::Chapter(program.clone(), position)
                        });
                        self.chapter_notice = "チャプター再生を準備します（成功後に切替）".into();
                    }
                }
            }
            Err(reason) => self.chapter_notice = reason.into(),
        }
        Ok(())
    }
    fn key_browser(&mut self, key: KeyEvent, service: &mut Service) -> Result<bool, &'static str> {
        match key.code {
            KeyCode::Char('q') => return Ok(true),
            // 通常画面の Esc は終了しない。出ている通知は消すだけ。無いときは終了案内を出す。
            // 停止文・受信待ち・再接続中も通知なので消し、同じ Esc では案内に替えない。
            KeyCode::Esc => {
                if self.has_footer_notice() {
                    self.dismiss_notices();
                } else {
                    // 5秒保持にすると、直後の受信待ち・再接続中・追いつきが隠れる。
                    self.playback_notice = super::QUIT_HINT.into();
                    self.notice_hold = super::NoticeHold::Other;
                    self.notice_hold_until = None;
                }
                return Ok(false);
            }
            KeyCode::Char('?') => {
                self.help_open = true;
                self.help_scroll = scroll::Scroll::default();
            }
            KeyCode::Char('e') => {
                self.failures_open = true;
                self.failure_list = ListState::default();
                if failures::count(&self.app) > 0 {
                    self.failure_list.select(Some(0));
                }
            }
            KeyCode::Char('a') => {
                if self.offline {
                    self.show_notice("オフライン確認ではセッションを通信確認しません");
                } else {
                    self.auth_refresh = true;
                }
            }
            KeyCode::Char('O') => {
                if self.offline {
                    self.show_notice("オフライン確認ではログアウトしません");
                } else {
                    self.logout_confirm = true;
                }
            }
            KeyCode::Char('I') => {
                if self.offline {
                    self.show_notice("オフライン確認ではログインしません");
                } else {
                    self.login = Some(login::Editor::default());
                }
            }
            KeyCode::Char('b') => {
                if self.offline {
                    self.show_notice("オフライン確認では保存条件を読み書きしません");
                } else {
                    self.named_generation = self
                        .named_generation
                        .checked_add(1)
                        .ok_or("保存条件の表示番号の上限です")?;
                    self.named = Some(named::Manager::new(ViewId(self.named_generation)));
                    self.named_action = Some(NamedOperation::List);
                }
            }
            KeyCode::Char('f') => {
                self.app.begin_edit();
                self.filters = Some(filters::Editor::new(
                    &self.app.committed,
                    &self.app.programs,
                ));
            }
            KeyCode::Char('c') => {
                if self.offline {
                    self.show_notice("オフライン確認ではチャプターを取得しません");
                } else if let Some(target) = self.selected().cloned() {
                    self.open_chapters(target)?;
                }
            }
            KeyCode::Char('/') => {
                self.app.begin_edit();
                self.input = Some(
                    self.app
                        .committed
                        .filter()
                        .keyword()
                        .unwrap_or_default()
                        .into(),
                );
            }
            KeyCode::Char('v') if self.app.chapters.target.is_none() => {
                self.toggle_live(service)?
            }
            KeyCode::Char('r') => self.submit(service)?,
            KeyCode::Down
            | KeyCode::Char('j')
            | KeyCode::Up
            | KeyCode::Char('k')
            | KeyCode::PageDown
            | KeyCode::PageUp
            | KeyCode::Home
            | KeyCode::End => {
                let count = self.rows().len();
                viewport::navigate(&mut self.list, count, self.list_height, key.code);
            }
            KeyCode::Enter => {
                if self.offline {
                    self.show_notice("オフライン確認では再生しません");
                } else if let Some(program) = self.selected() {
                    let request = if self.live_mode {
                        playback::live_request
                    } else {
                        playback::request
                    };
                    match request(program, Utc::now().fixed_offset()) {
                        Ok(_) => {
                            self.playback_action = Some(if self.live_mode {
                                PlaybackAction::Live(program.clone())
                            } else {
                                PlaybackAction::Select(program.clone())
                            })
                        }
                        Err(reason) => self.show_notice(reason),
                    }
                }
            }
            KeyCode::Char('l') if self.app.chapters.target.is_none() => {
                if self.offline {
                    self.show_notice("オフライン確認では再生しません");
                } else if let Some(program) = self.selected() {
                    match playback::live_request(program, Utc::now().fixed_offset()) {
                        Ok(_) => self.playback_action = Some(PlaybackAction::Live(program.clone())),
                        Err(reason) => self.show_notice(reason),
                    }
                }
            }
            KeyCode::Char('g') => {
                if let Some(snapshot) = &self.app.playback {
                    match seek::Editor::new(snapshot) {
                        Ok(editor) => self.seek = Some(editor),
                        Err(reason) => self.show_notice(reason),
                    }
                } else {
                    self.show_notice("再生中の番組はありません");
                }
            }
            KeyCode::Char('s') => self.playback_action = Some(PlaybackAction::Stop),
            KeyCode::Char(' ') => {
                if let Some(snapshot) = &self.app.playback {
                    self.playback_action = Some(PlaybackAction::Command(
                        if snapshot.state == PlaybackState::Paused {
                            Command::Resume
                        } else {
                            Command::Pause
                        },
                    ));
                }
            }
            KeyCode::Left | KeyCode::Right if self.app.playback.is_some() => {
                self.playback_action = Some(PlaybackAction::Command(Command::Relative(
                    if key.code == KeyCode::Left { -15 } else { 15 },
                )));
            }
            KeyCode::Char('+') | KeyCode::Char('-') => {
                let volume = self.volume;
                let volume = if key.code == KeyCode::Char('+') {
                    (volume + 5).min(100)
                } else {
                    volume.saturating_sub(5)
                };
                self.playback_action = Some(PlaybackAction::Command(Command::Volume(volume)));
            }
            _ => (),
        }
        Ok(false)
    }
    pub(super) fn control_hint(&self) -> String {
        if self.login.is_some() {
            return "Tab 入力欄 / Enter 次の欄・ログイン確定 / Ctrl+U 消去 / Esc 取消 / Ctrl+C 終了"
                .into();
        }
        if self.logout_confirm {
            return concat!(
                "ログアウトしますか？ 再生を停止し保存セッションを削除 / y 確定 / n / ",
                dialog_close_hint!()
            )
            .into();
        }
        if let Some(editor) = &self.seek {
            return editor.prompt();
        }
        if let Some(manager) = &self.named {
            return manager.controls().into();
        }
        if let Some(editor) = &self.filters {
            return editor.controls().into();
        }
        let enter = if self.app.chapters.target.is_some() {
            if self
                .app
                .chapters
                .jump(self.chapter_list.selected().unwrap_or_default())
                .is_ok()
            {
                "Enter チャプターから再生 / r 再取得 / Esc 詳細へ"
            } else {
                "Enter ジャンプ不可（位置未検証）"
            }
        } else if self.live_mode {
            "Enter ライブ再生 / v TF画面 / f 条件 / b 保存条件 / c チャプター"
        } else {
            "Enter TF再生 / v ライブ画面 / l ライブ / f 条件 / b 保存条件 / c チャプター"
        };
        let tail = if self.app.chapters.target.is_some() {
            "q・Esc 詳細へ / Ctrl+C 終了"
        } else {
            "Esc 通知を消す・終了案内 / q・Ctrl+C 終了"
        };
        format!(
            "? 操作一覧 / e 取得状況 / {enter} / Space 一時停止・再開 / s 停止 / ←→ 15秒 / g 位置 / +- 音量 / {tail}"
        )
    }
}
