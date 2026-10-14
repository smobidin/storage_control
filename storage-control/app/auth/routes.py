from flask import Blueprint, render_template, redirect, url_for, request, flash
from flask_login import login_user, logout_user, login_required, current_user
from flask_wtf import FlaskForm
from wtforms import StringField, PasswordField, SubmitField
from wtforms.validators import DataRequired
from argon2 import PasswordHasher
from argon2.exceptions import VerifyMismatchError
from ..extensions import db
from ..models import User

bp = Blueprint("auth", __name__, template_folder="../../templates")


class LoginForm(FlaskForm):
    username = StringField("Username", validators=[DataRequired()])
    password = PasswordField("Password", validators=[DataRequired()])
    submit = SubmitField("Sign in")


@bp.route("/login", methods=["GET", "POST"])
def login():
    form = LoginForm()
    if form.validate_on_submit():
        user = User.query.filter_by(username=form.username.data).first()
        if user:
            try:
                PasswordHasher().verify(user.password_hash, form.password.data)
                login_user(user)
                return redirect(request.args.get("next") or url_for("dashboard.index"))
            except VerifyMismatchError:
                pass
        flash("Invalid credentials", "danger")
    return render_template("auth/login.html", form=form)


@bp.route("/logout", methods=["POST"])
@login_required
def logout():
    logout_user()
    return redirect(url_for("auth.login"))
