//! Help with S3 credentials wherever an S3 error shows (the S3 browser, a tab
//! that couldn't open, the SQL console, a comparison, notifications): choose a
//! profile, set one up, or sign in again with `aws sso login`, then retry what
//! failed.

use std::process::Stdio;
use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
pub use parquetry_engine::CredentialIssue;

use crate::app_state::AppState;

/// What to do once the credentials change (a profile was chosen, or signing in
/// finished): usually, try again.
pub type Retry = Rc<dyn Fn(&mut Window, &mut App)>;

const AWS_SSO_GUIDE: &str = "https://docs.aws.amazon.com/cli/latest/userguide/sso-configure-profile-token.html";
const AWS_CLI_INSTALL: &str = "https://docs.aws.amazon.com/cli/latest/userguide/getting-started-install.html";

/// The credentials problem an error message describes, if any.
pub fn issue(message: &str) -> Option<CredentialIssue> {
    parquetry_engine::credential_issue(message)
}

/// `aws sso login` in progress (one for the whole app), or why the last one failed.
#[derive(Default)]
struct SignIn {
    running: Option<(u32, Task<()>)>,
    failed: Option<SharedString>,
}

impl Global for SignIn {}

fn sign_in_state(cx: &App) -> Option<&SignIn> {
    cx.try_global::<SignIn>()
}

/// The AWS profile in use (`None`: the default credentials).
pub fn current_profile(cx: &App) -> Option<String> {
    AppState::settings(cx).engine.s3.profile.clone().filter(|p| !p.is_empty())
}

/// Switch to `profile`, then `retry`.
pub fn use_profile(profile: String, retry: &Retry, window: &mut Window, cx: &mut App) {
    AppState::update_settings(cx, |s| s.engine.s3.profile = Some(profile));
    retry(window, cx);
}

/// Run `aws sso login` for the current profile; once it succeeds, pick up the
/// new credentials and `retry` (in the window that asked).
pub fn sign_in(retry: Retry, window: &mut Window, cx: &mut App) {
    if sign_in_state(cx).is_some_and(|s| s.running.is_some()) {
        return;
    }
    let Some(aws) = crate::programs::find("aws") else { return };
    let mut command = crate::programs::command(&aws);
    command.args(["sso", "login"]);
    if let Some(profile) = current_profile(cx) {
        command.args(["--profile", &profile]);
    }
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            cx.default_global::<SignIn>().failed = Some(format!("Couldn’t run the AWS CLI: {error}").into());
            cx.refresh_windows();
            return;
        }
    };
    let pid = child.id();
    let handle = window.window_handle();
    // Cancel (or quitting) drops this task.
    let task = cx.spawn(async move |cx: &mut AsyncApp| {
        let output = cx.background_executor().spawn(async move { child.wait_with_output() }).await;
        cx.update(|cx| {
            let state = cx.default_global::<SignIn>();
            state.running = None;
            state.failed = match &output {
                Ok(output) if output.status.success() => None,
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let reason = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("Signing in didn’t finish.");
                    Some(reason.trim().to_string().into())
                }
                Err(error) => Some(error.to_string().into()),
            };
            if state.failed.is_none() {
                AppState::engine(cx).refresh_s3_credentials();
                let _ = handle.update(cx, |_, window, cx| retry(window, cx));
            }
            cx.refresh_windows();
        });
    });
    let state = cx.default_global::<SignIn>();
    state.running = Some((pid, task));
    state.failed = None;
    cx.refresh_windows();
}

/// Stop a sign-in in progress (Cancel, or quitting).
pub fn cancel_sign_in(cx: &mut App) {
    if let Some((pid, _task)) = cx.default_global::<SignIn>().running.take() {
        crate::programs::stop(pid);
    }
    cx.refresh_windows();
}

fn profiles_except_current(cx: &App) -> Vec<String> {
    let current = current_profile(cx);
    crate::dialogs::settings::aws_profiles().into_iter().filter(|p| Some(p) != current.as_ref()).collect()
}

fn title(issue: CredentialIssue) -> &'static str {
    match issue {
        CredentialIssue::Missing => "Connect to AWS",
        CredentialIssue::SignedOut => "Sign in to AWS",
        CredentialIssue::Denied => "No access to this S3 location",
    }
}

fn body(issue: CredentialIssue, others: &[String], cx: &App) -> String {
    let profile = current_profile(cx);
    match issue {
        CredentialIssue::Missing if others.is_empty() => {
            "Parquetry reads S3 with your AWS CLI profiles, and none are set up on this computer yet. Set one up in Terminal, then come back.".into()
        }
        CredentialIssue::Missing => "Parquetry reads S3 with your AWS CLI profiles. Choose one to continue.".into(),
        CredentialIssue::SignedOut => match &profile {
            Some(p) => format!("Your AWS sign-in for the “{p}” profile has expired."),
            None => "Your AWS sign-in has expired.".into(),
        },
        CredentialIssue::Denied => {
            let who = profile.map(|p| format!("The “{p}” profile")).unwrap_or_else(|| "Your current AWS credentials".into());
            if others.is_empty() {
                format!("{who} can’t read this location. Ask for access, or set up a profile that can.")
            } else {
                format!("{who} can’t read this location. Choose another profile, or ask for access.")
            }
        }
    }
}

/// The buttons that fix `issue`.
fn actions(issue: CredentialIssue, others: &[String], retry: &Retry, cx: &App) -> AnyElement {
    let aws = crate::programs::find("aws");
    let link = |id: &'static str, label: &'static str, url: &'static str| {
        Button::new(id).label(label).icon(Lucide::ExternalLink).small().ghost().on_click(move |_, _, cx| cx.open_url(url))
    };
    let copy = |id: &'static str, label: &'static str, text: String| {
        Button::new(id)
            .label(label)
            .icon(Lucide::Copy)
            .small()
            .outline()
            .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.clone())))
    };
    let row = || h_flex().gap_2().flex_wrap().justify_center();
    let setup = || {
        row().child(copy("copy-configure", "Copy setup command", "aws configure sso".into())).child(match aws {
            Some(_) => link("sso-guide", "How to set up", AWS_SSO_GUIDE),
            None => link("install-aws", "Install the AWS CLI", AWS_CLI_INSTALL),
        })
    };
    match issue {
        CredentialIssue::Missing | CredentialIssue::Denied if others.is_empty() => setup().into_any_element(),
        CredentialIssue::Missing | CredentialIssue::Denied => row()
            .children(others.iter().take(4).enumerate().map(|(ix, p)| {
                let (p, retry) = (p.clone(), retry.clone());
                Button::new(("use-profile", ix))
                    .label(format!("Use {p}"))
                    .small()
                    .map(|b| if ix == 0 { b.primary() } else { b.outline() })
                    .on_click(move |_, window, cx| use_profile(p.clone(), &retry, window, cx))
            }))
            .into_any_element(),
        CredentialIssue::SignedOut => {
            let running = sign_in_state(cx).is_some_and(|s| s.running.is_some());
            match (running, aws) {
                (true, _) => h_flex()
                    .gap_2()
                    .child(Spinner::new().small())
                    .child(div().text_sm().child("Finish signing in in your browser…"))
                    .child(Button::new("cancel-sign-in").label("Cancel").small().ghost().on_click(|_, _, cx| cancel_sign_in(cx)))
                    .into_any_element(),
                (false, Some(_)) => {
                    let retry = retry.clone();
                    Button::new("sign-in")
                        .label("Sign In")
                        .icon(Lucide::LogIn)
                        .small()
                        .primary()
                        .on_click(move |_, window, cx| sign_in(retry.clone(), window, cx))
                        .into_any_element()
                }
                (false, None) => {
                    let command = match current_profile(cx) {
                        Some(p) => format!("aws sso login --profile {p}"),
                        None => "aws sso login".into(),
                    };
                    row()
                        .child(copy("copy-login", "Copy sign-in command", command))
                        .child(link("install-aws", "Install the AWS CLI", AWS_CLI_INSTALL))
                        .into_any_element()
                }
            }
        }
    }
}

/// A panel in place of an error: what's wrong with the credentials and buttons
/// that fix it. `retry` runs after a profile is chosen or signing in finishes;
/// `footer` adds context below (what failed, a Close button).
pub fn panel(issue: CredentialIssue, retry: Retry, footer: Option<AnyElement>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let others = profiles_except_current(cx);
    let icon = match issue {
        CredentialIssue::Missing => Lucide::KeyRound,
        CredentialIssue::SignedOut => Lucide::LogIn,
        CredentialIssue::Denied => Lucide::ShieldX,
    };
    let note: Option<SharedString> = match issue {
        CredentialIssue::SignedOut => sign_in_state(cx).and_then(|s| s.failed.clone()),
        CredentialIssue::Missing => Some("Public buckets open without credentials: type a path such as s3://bucket/prefix/.".into()),
        CredentialIssue::Denied => None,
    };
    v_flex()
        .flex_1()
        .min_h_0()
        .w_full()
        .items_center()
        .justify_center()
        .gap_3()
        .p_6()
        .child(Icon::new(icon).large().text_color(theme.primary))
        .child(div().text_base().font_weight(FontWeight::SEMIBOLD).child(title(issue)))
        .child(
            div()
                .max_w(rems(30.))
                .text_sm()
                .text_center()
                .text_color(theme.muted_foreground)
                .whitespace_normal()
                .child(body(issue, &others, cx)),
        )
        .child(actions(issue, &others, &retry, cx))
        .when_some(note, |this, note| {
            this.child(div().max_w(rems(30.)).text_xs().text_center().text_color(theme.muted_foreground).whitespace_normal().child(note))
        })
        .when_some(footer, |this, footer| this.child(div().mt_4().child(footer)))
        .into_any_element()
}

/// A notification for `issue` with a button that fixes it: Sign In, or
/// Settings (to choose a profile). `retry` runs once signing in finishes.
pub fn notification(issue: CredentialIssue, retry: Retry, cx: &App) -> Notification {
    let others = profiles_except_current(cx);
    Notification::warning(body(issue, &others, cx)).title(title(issue)).action(move |_, _, _| match issue {
        CredentialIssue::SignedOut if crate::programs::find("aws").is_some() => {
            let retry = retry.clone();
            Button::new("notification-sign-in").label("Sign In").small().primary().on_click(move |_, window, cx| sign_in(retry.clone(), window, cx))
        }
        _ => Button::new("notification-settings")
            .label("Settings…")
            .small()
            .primary()
            .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::actions::OpenSettings), cx)),
    })
}
