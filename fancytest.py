import unittest
import traceback
from IPython.display import display, Markdown, HTML
from IPython.core.interactiveshell import InteractiveShell
from html import escape

class FancyTestResult(unittest.TextTestResult):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.results = []

    def addSuccess(self, test):
        super().addSuccess(test)
        self.results.append(('pass', test, None))

    def addFailure(self, test, err):
        super().addFailure(test, err)
        self.results.append(('fail', test, err))

    def addError(self, test, err):
        super().addError(test, err)
        self.results.append(('error', test, err))


class FancyTestRunner(unittest.TextTestRunner):
    def __init__(self, **kwargs):
        super().__init__(resultclass=FancyTestResult, stream=open('/dev/null', 'w'), **kwargs)

    def run(self, test):
        ip = InteractiveShell.instance()
        
        result = super().run(test)
        display(Markdown(f"## 🧪 **Test Results**: {result.testsRun} run"))

        for status, test_case, err in result.results:
            test_name = f"`{test_case}`"
            if status == 'pass':
                display(Markdown(f"- ✅ **PASS** {test_name}"))
            elif status == 'fail':
                display(Markdown(f"- ❌ **FAIL** {test_name}"))
                display(Markdown("#### Traceback"))
                ip.showtraceback(exc_tuple=err)
            elif status == 'error':
                display(Markdown(f"- 💥 **ERROR** {test_name}"))
                display(Markdown("#### Traceback"))
                ip.showtraceback(exc_tuple=err)

        return result

def run():
    return FancyTestRunner().run(
        unittest.TestLoader().loadTestsFromTestCase(Tests)
    )
