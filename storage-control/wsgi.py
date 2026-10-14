import os
from app.config import DATA_DIR

# Ensure dirs
DATA_DIR.mkdir(parents=True, exist_ok=True)

from app import create_app

app = create_app()
