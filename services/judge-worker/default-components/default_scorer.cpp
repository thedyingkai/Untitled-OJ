#include <algorithm>
#include <fstream>
#include <iostream>
#include <regex>
#include <sstream>
#include <string>

static std::string read_all(const std::string& path) {
    std::ifstream in(path, std::ios::binary);
    std::ostringstream ss;
    ss << in.rdbuf();
    return ss.str();
}

int main(int argc, char** argv) {
    if (argc < 2) {
        std::cerr << "usage: default_scorer <case_results.json> [score.json]\n";
        return 2;
    }

    const std::string text = read_all(argv[1]);
    const std::regex score_re("\"score\"\\s*:\\s*(-?[0-9]+)");
    int sum = 0;
    for (auto it = std::sregex_iterator(text.begin(), text.end(), score_re);
         it != std::sregex_iterator(); ++it) {
        int score = 0;
        try {
            score = std::stoi((*it)[1].str());
        } catch (...) {
            score = 0;
        }
        sum += std::max(0, score);
    }
    sum = std::min(sum, 100);

    std::cout << "{\"status\":\"ACCEPTED\",\"score\":" << sum
              << ",\"message\":\"sum capped at 100\"}\n";
    return 0;
}
