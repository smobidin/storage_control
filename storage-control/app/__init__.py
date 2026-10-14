import os
from flask import Flask, render_template
from .extensions import db, login_manager, csrf, migrate
from .config import Config, DATA_DIR, RUNS_DIR


def create_app(config_class=Config):
    app = Flask(__name__)
    app.config.from_object(config_class)

    DATA_DIR.mkdir(parents=True, exist_ok=True)
    RUNS_DIR.mkdir(parents=True, exist_ok=True)

    db.init_app(app)
    login_manager.init_app(app)
    csrf.init_app(app)
    migrate.init_app(app, db)

    login_manager.login_view = "auth.login"

    from .models import User

    @login_manager.user_loader
    def load_user(user_id):
        return User.query.get(int(user_id))

    # register blueprints
    from .auth.routes import bp as auth_bp
    from .dashboard.routes import bp as dashboard_bp
    from .services.routes import bp as services_bp
    from .backups.routes import bp as backups_bp
    from .planner.routes import bp as scheduler_bp
    from .updates.routes import bp as updates_bp
    app.register_blueprint(auth_bp)
    app.register_blueprint(dashboard_bp)
    app.register_blueprint(services_bp)
    app.register_blueprint(backups_bp)
    app.register_blueprint(scheduler_bp)
    app.register_blueprint(updates_bp)

    with app.app_context():
        db.create_all()
        _ensure_admin(app)

    # Seed at least one SMART baseline shortly after startup so Dashboard alarms
    # are useful before the nightly check runs.
    try:
        from .runners.common import async_call
        from .runners.smart_monitor import run_smart_check
        async_call(run_smart_check)
    except Exception as e:
        from flask import Flask as _unused
        app.logger.warning("smart monitor startup check failed: %s", e)

    try:
        from .scheduler import init_scheduler
        init_scheduler(app)
    except Exception as e:
        app.logger.warning("scheduler init failed: %s", e)

    @app.errorhandler(404)
    def not_found(_):
        return render_template("errors/404.html"), 404

    return app


def _ensure_admin(app):
    from .models import User
    from argon2 import PasswordHasher
    username = app.config["ADMIN_USERNAME"]
    password = app.config["ADMIN_PASSWORD"]
    if not password:
        return
    ph = PasswordHasher()
    u = User.query.filter_by(username=username).first()
    if u is None:
        u = User(username=username, password_hash=ph.hash(password))
        db.session.add(u)
    else:
        u.password_hash = ph.hash(password)
    db.session.commit()
