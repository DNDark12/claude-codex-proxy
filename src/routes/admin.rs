// src/routes/admin.rs
//
// Serve HTML pages cho dashboard.
// Dùng include_str! để embed HTML vào binary — không cần static/ folder ở runtime.
//
// Routes:
//   GET /           → dashboard (index.html)
//   GET /accounts   → account manager
//   GET /sessions   → sessions + jobs viewer

use warp::Filter;

pub fn admin_routes() -> impl Filter<Extract = impl warp::Reply, Error = warp::Rejection> + Clone {
    // GET / → dashboard
    let index = warp::path::end().and(warp::get()).map(|| {
        warp::reply::with_header(
            include_str!("../static/index.html"),
            "content-type",
            "text/html; charset=utf-8",
        )
    });

    // GET /accounts
    let accounts = warp::path("accounts")
        .and(warp::path::end())
        .and(warp::get())
        .map(|| {
            warp::reply::with_header(
                include_str!("../static/accounts.html"),
                "content-type",
                "text/html; charset=utf-8",
            )
        });

    // GET /sessions
    let sessions = warp::path("sessions")
        .and(warp::path::end())
        .and(warp::get())
        .map(|| {
            warp::reply::with_header(
                include_str!("../static/sessions.html"),
                "content-type",
                "text/html; charset=utf-8",
            )
        });

    index.or(accounts).or(sessions)
}
