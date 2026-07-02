#include <cstdlib>
#include <fstream>
#include <iostream>
#include <string>

static std::string shell_quote(const std::string& s) {
    std::string out = "'";
    for (char c : s) {
        if (c == '\'') {
            out += "'\\''";
        } else {
            out.push_back(c);
        }
    }
    out.push_back('\'');
    return out;
}

static void write_report(const std::string& path, const std::string& status, int score, const std::string& message) {
    std::ofstream out(path, std::ios::binary);
    out << "{\"status\":\"" << status << "\",\"score\":" << score
        << ",\"message\":\"" << message << "\"}\n";
}

int main(int argc, char** argv) {
    if (argc < 8) {
        std::cerr << "usage: default_runner <input> <answer> <submission> <stdout> <stderr> <report> <language>\n";
        return 2;
    }

    const std::string input = argv[1];
    const std::string submission = argv[3];
    const std::string stdout_path = argv[4];
    const std::string stderr_path = argv[5];
    const std::string report_path = argv[6];
    const std::string language = argv[7];

    std::string command;
    if (language.find("python") != std::string::npos || language == "py" || language == "py3") {
        command = "python3 " + shell_quote(submission);
    } else if (language.find("java") != std::string::npos) {
        command = "java -cp . Main";
    } else {
        command = "./" + shell_quote(submission);
    }

    command += " < " + shell_quote(input);
    command += " > " + shell_quote(stdout_path);
    command += " 2> " + shell_quote(stderr_path);

    const int rc = std::system(command.c_str());
    if (rc == 0) {
        write_report(report_path, "ACCEPTED", 100, "runner completed");
        return 0;
    }

    write_report(report_path, "RUNTIME_ERROR", 0, "submission exited with non-zero status");
    return 1;
}
