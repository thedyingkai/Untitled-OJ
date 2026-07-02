#include <algorithm>
#include <fstream>
#include <iostream>
#include <sstream>
#include <string>
#include <vector>

static std::string read_all(const std::string& path) {
    std::ifstream in(path, std::ios::binary);
    std::ostringstream ss;
    ss << in.rdbuf();
    return ss.str();
}

static std::vector<std::string> normalize(const std::string& input) {
    std::string s;
    s.reserve(input.size());
    for (size_t i = 0; i < input.size(); ++i) {
        if (input[i] == '\r') {
            if (i + 1 < input.size() && input[i + 1] == '\n') {
                continue;
            }
            s.push_back('\n');
        } else {
            s.push_back(input[i]);
        }
    }

    std::vector<std::string> lines;
    std::string line;
    std::istringstream stream(s);
    while (std::getline(stream, line, '\n')) {
        while (!line.empty() && (line.back() == ' ' || line.back() == '\t')) {
            line.pop_back();
        }
        lines.push_back(line);
    }
    if (!s.empty() && s.back() == '\n') {
        lines.push_back("");
    }
    while (!lines.empty() && lines.back().empty()) {
        lines.pop_back();
    }
    return lines;
}

int main(int argc, char** argv) {
    if (argc < 4) {
        std::cerr << "usage: default_checker <input> <answer> <output> [report]\n";
        return 2;
    }

    const std::string answer = read_all(argv[2]);
    const std::string output = read_all(argv[3]);
    if (normalize(answer) == normalize(output)) {
        std::cout << "{\"status\":\"ACCEPTED\",\"message\":\"accepted\"}\n";
        return 0;
    }

    std::cout << "{\"status\":\"WRONG_ANSWER\",\"score\":0,\"message\":\"wrong answer\"}\n";
    return 1;
}
