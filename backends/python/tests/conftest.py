# SPDX-License-Identifier: Apache-2.0
import pytest


@pytest.fixture
def anyio_backend() -> str:
    return "asyncio"
