from dx._env import Environment, detect_environment


def test_detect_plain_python_when_no_ipython(monkeypatch):
    monkeypatch.setattr("dx._env._get_ipython", lambda: None)
    assert detect_environment() == Environment.PLAIN_PYTHON


def test_detect_ipython_without_kernel(monkeypatch):
    class FakeIPython:
        kernel = None

    monkeypatch.setattr("dx._env._get_ipython", lambda: FakeIPython())
    assert detect_environment() == Environment.IPYTHON_NO_KERNEL


def test_detect_ipykernel(monkeypatch):
    class FakeKernel:
        pass

    class FakeIPython:
        kernel = FakeKernel()

    monkeypatch.setattr("dx._env._get_ipython", lambda: FakeIPython())
    assert detect_environment() == Environment.IPYKERNEL
