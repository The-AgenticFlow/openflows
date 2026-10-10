//! Minimal server-rendered pages for CLI onboarding.
//!
//! A general dashboard is outside this package; these pages are only enough
//! for a person to: be redirected to GitHub login, verify/approve a device
//! request, and accept an invitation. All user-supplied values are HTML-escaped
//! before interpolation, and mutations are POST-only with CSRF tokens.

/// HTML-escape a value for safe interpolation into markup.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// The device verification/approval page. Shows the human code and a POST
/// approve form carrying a CSRF token. The server must never approve via GET.
pub fn device_verify_page(code: &str, csrf: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Openflows CLI approval</title>\
         <style>body{{font-family:system-ui,sans-serif;max-width:32rem;margin:4rem auto;padding:0 1rem}}\
         .code{{font-size:2rem;letter-spacing:.2em;font-weight:600;margin:.5rem 0}}button{{padding:.6rem 1.4rem;font-size:1rem}}</style>\
         </head><body><h1>Authorize Openflows CLI</h1>\
         <p>Enter this code to approve the CLI sign-in:</p>\
         <div class=\"code\">{}</div>\
         <form method=\"post\" action=\"/auth/cli/approve\">\
         <input type=\"text\" name=\"code\" value=\"{}\">\
         <input type=\"hidden\" name=\"_csrf\" value=\"{}\">\
         <button type=\"submit\">Approve</button></form>\
         </body></html>",
        esc(code),
        esc(code),
        esc(csrf)
    )
}

/// A page shown after a device request was already handled or is invalid.
pub fn device_result_page(message: &str, status: u16) -> (u16, String) {
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Openflows</title></head>\
         <body><p>{}</p></body></html>",
        esc(message)
    );
    (status, html)
}

/// A page telling a user they must sign in before approving a device.
pub fn device_needs_login_page(login_url: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Openflows</title></head>\
         <body><h1>Sign in required</h1><p><a href=\"{}\">Sign in with GitHub</a> to approve the CLI request.</p></body></html>",
        esc(login_url)
    )
}

/// The invitation acceptance page. POST-only acceptance carries a CSRF token.
pub fn invitation_page(org_display: &str, role: &str, csrf: &str, token: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Openflows invitation</title>\
         <style>body{{font-family:system-ui,sans-serif;max-width:32rem;margin:4rem auto;padding:0 1rem}}\
         button{{padding:.6rem 1.4rem;font-size:1rem}}</style>\
         </head><body><h1>Invitation</h1>\
         <p>You have been invited to join <strong>{}</strong> as {}.</p>\
         <form method=\"post\" action=\"/invitations/accept\">\
         <input type=\"hidden\" name=\"_csrf\" value=\"{}\">\
         <input type=\"hidden\" name=\"token\" value=\"{}\">\
         <button type=\"submit\">Accept invitation</button></form>\
         </body></html>",
        esc(org_display),
        esc(role),
        esc(csrf),
        esc(token)
    )
}

/// A simple login landing page linking to the GitHub authorization flow.
pub fn login_page(authorize_url: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Openflows sign in</title>\
         <style>body{{font-family:system-ui,sans-serif;max-width:32rem;margin:4rem auto;padding:0 1rem}}\
         a{{display:inline-block;padding:.7rem 1.5rem;background:#1b6ef3;color:#fff;border-radius:6px;text-decoration:none}}</style>\
         </head><body><h1>Openflows</h1><p>Sign in to manage your organizations.</p>\
         <a href=\"{}\">Sign in with GitHub</a></body></html>",
        esc(authorize_url)
    )
}
